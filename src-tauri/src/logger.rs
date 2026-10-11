use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::collections::VecDeque;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

const MAX_LINES: usize = 1000;
const LOG_FILE_NAME: &str = "mula.log";

fn config_dir() -> PathBuf {
    dirs::config_dir()
        .map(|p| p.join("mula"))
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn logs_dir() -> PathBuf {
    config_dir().join("logs")
}

pub fn log_path() -> PathBuf {
    logs_dir().join(LOG_FILE_NAME)
}

fn write_log_lines(path: &Path, lines: &VecDeque<String>) -> Result<(), std::io::Error> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = fs::File::create(path)?;
    for line in lines {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

fn format_record(record: &Record) -> String {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    format!(
        "[{}] [{}] {}",
        ts,
        record.level(),
        record.args()
    )
}

pub struct AppLogger {
    sender: Mutex<Option<Sender<String>>>,
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LevelFilter::Info
    }

    fn log(&self, record: &Record) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let msg = format_record(record);
        if let Ok(sender) = self.sender.lock() {
            if let Some(tx) = sender.as_ref() {
                let _ = tx.send(msg);
            }
        }
    }

    fn flush(&self) {}
}

pub fn init() -> Result<(), log::SetLoggerError> {
    let (tx, rx) = channel::<String>();
    let path = log_path();

    // Load any existing log file so we keep history across restarts.
    let mut initial_lines: VecDeque<String> = VecDeque::with_capacity(MAX_LINES + 1);
    if path.exists() {
        if let Ok(content) = fs::read_to_string(&path) {
            for line in content.lines() {
                if !line.trim().is_empty() {
                    initial_lines.push_back(line.to_string());
                }
            }
        }
    }
    while initial_lines.len() > MAX_LINES {
        initial_lines.pop_front();
    }

    thread::spawn(move || {
        let mut lines = initial_lines;
        let _ = write_log_lines(&path, &lines);
        loop {
            match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(msg) => {
                    lines.push_back(msg);
                    while lines.len() > MAX_LINES {
                        lines.pop_front();
                    }
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            if !lines.is_empty() {
                let _ = write_log_lines(&path, &lines);
            }
        }
        // Final flush before the thread exits
        if !lines.is_empty() {
            let _ = write_log_lines(&path, &lines);
        }
    });

    log::set_boxed_logger(Box::new(AppLogger {
        sender: Mutex::new(Some(tx)),
    }))
    .map(|()| log::set_max_level(LevelFilter::Info))
}

/// Log a raw message from the frontend or other non-log-crate sources.
/// Level is one of: trace, debug, info, warn, error.
pub fn log_message(level: &str, message: &str) {
    let level = match level.to_lowercase().as_str() {
        "trace" => Level::Trace,
        "debug" => Level::Debug,
        "info" => Level::Info,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    };
    log::log!(level, "{}", message);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_are_formatted_with_timestamp_level_and_message() {
        let line = format_record(&Record::builder().args(format_args!("disk {} ok", 3)).level(Level::Warn).build());
        assert!(line.ends_with("] [WARN] disk 3 ok"), "{line}");
        assert!(line.starts_with('['));
        // [YYYY-MM-DD HH:MM:SS.mmm]
        assert_eq!(line.find(']'), Some(24));
    }

    #[test]
    fn log_lines_are_written_one_per_line_creating_folders() {
        let dir = crate::test_support::TempDir::new("logger");
        let path = dir.join("logs").join("mula.log");
        let lines: VecDeque<String> = ["first".to_string(), "second".to_string()].into();
        write_log_lines(&path, &lines).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "first\nsecond\n");

        // Rewrites replace the file instead of appending.
        write_log_lines(&path, &VecDeque::from(["only".to_string()])).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "only\n");
    }

    #[test]
    fn only_info_and_more_severe_records_are_enabled() {
        let logger = AppLogger { sender: Mutex::new(None) };
        let meta = |level| Metadata::builder().level(level).build();
        assert!(logger.enabled(&meta(Level::Error)));
        assert!(logger.enabled(&meta(Level::Info)));
        assert!(!logger.enabled(&meta(Level::Debug)));
    }

    #[test]
    fn log_file_lives_in_the_mula_logs_folder() {
        let path = log_path();
        assert!(path.ends_with(Path::new("mula").join("logs").join("mula.log")));
    }
}
