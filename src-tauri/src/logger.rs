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
