//! The app log: `%APPDATA%\mula\logs\mula.log`, appended to and rotated to `mula.log.1` once it
//! grows past [`MAX_LOG_BYTES`].

use chrono::Local;
use log::{Level, LevelFilter, Log, Metadata, Record};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Sender};
use std::thread;

const MAX_LOG_BYTES: u64 = 1024 * 1024;
const LOG_FILE_NAME: &str = "mula.log";

fn log_path() -> PathBuf {
    crate::util::mula_config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("logs")
        .join(LOG_FILE_NAME)
}

fn format_record(record: &Record) -> String {
    let ts = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    format!("[{}] [{}] {}", ts, record.level(), record.args())
}

/// An append-only log file that moves itself to `<name>.1` once it exceeds `max_bytes`.
struct LogFile {
    path: PathBuf,
    max_bytes: u64,
    file: Option<BufWriter<File>>,
    size: u64,
}

impl LogFile {
    fn new(path: PathBuf, max_bytes: u64) -> Self {
        Self { path, max_bytes, file: None, size: 0 }
    }

    fn open(&mut self) -> std::io::Result<&mut BufWriter<File>> {
        if self.file.is_none() {
            if let Some(parent) = self.path.parent() {
                fs::create_dir_all(parent)?;
            }
            let file = OpenOptions::new().create(true).append(true).open(&self.path)?;
            self.size = file.metadata()?.len();
            self.file = Some(BufWriter::new(file));
        }
        Ok(self.file.as_mut().expect("just opened"))
    }

    fn write_lines(&mut self, lines: &[String]) -> std::io::Result<()> {
        let file = self.open()?;
        let mut written = 0;
        for line in lines {
            writeln!(file, "{line}")?;
            written += line.len() as u64 + 1;
        }
        file.flush()?;
        self.size += written;
        if self.size > self.max_bytes {
            self.rotate()?;
        }
        Ok(())
    }

    fn rotate(&mut self) -> std::io::Result<()> {
        self.file = None;
        let mut rotated = self.path.as_os_str().to_owned();
        rotated.push(".1");
        let _ = fs::remove_file(&rotated);
        fs::rename(&self.path, rotated)?;
        self.size = 0;
        Ok(())
    }
}

pub struct AppLogger {
    sender: Sender<String>,
}

impl Log for AppLogger {
    fn enabled(&self, metadata: &Metadata) -> bool {
        metadata.level() <= LevelFilter::Info
    }

    fn log(&self, record: &Record) {
        if self.enabled(record.metadata()) {
            let _ = self.sender.send(format_record(record));
        }
    }

    fn flush(&self) {}
}

pub fn init() -> Result<(), log::SetLoggerError> {
    let (sender, receiver) = channel::<String>();
    let mut file = LogFile::new(log_path(), MAX_LOG_BYTES);

    thread::spawn(move || {
        // Block until something is logged, then write everything already queued in one go.
        while let Ok(first) = receiver.recv() {
            let mut batch = vec![first];
            batch.extend(receiver.try_iter());
            let _ = file.write_lines(&batch);
        }
    });

    log::set_boxed_logger(Box::new(AppLogger { sender })).map(|()| log::set_max_level(LevelFilter::Info))
}

/// Log a raw message from the frontend or other non-log-crate sources.
/// Level is one of: trace, debug, info, warn, error.
pub fn log_message(level: &str, message: &str) {
    let level = match level.to_lowercase().as_str() {
        "trace" => Level::Trace,
        "debug" => Level::Debug,
        "warn" => Level::Warn,
        "error" => Level::Error,
        _ => Level::Info,
    };
    log::log!(level, "{}", message);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::TempDir;
    use std::path::Path;

    #[test]
    fn records_are_formatted_with_timestamp_level_and_message() {
        let line = format_record(&Record::builder().args(format_args!("disk {} ok", 3)).level(Level::Warn).build());
        assert!(line.ends_with("] [WARN] disk 3 ok"), "{line}");
        assert!(line.starts_with('['));
        // [YYYY-MM-DD HH:MM:SS.mmm]
        assert_eq!(line.find(']'), Some(24));
    }

    #[test]
    fn lines_are_appended_creating_folders() {
        let dir = TempDir::new("logger");
        let path = dir.join("logs").join("mula.log");
        let mut log = LogFile::new(path.clone(), 1024);
        log.write_lines(&["first".to_string(), "second".to_string()]).unwrap();
        log.write_lines(&["third".to_string()]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\nthird\n");

        // A later session keeps appending to the same file.
        let mut again = LogFile::new(path.clone(), 1024);
        again.write_lines(&["fourth".to_string()]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\nsecond\nthird\nfourth\n");
    }

    #[test]
    fn the_log_rotates_once_it_grows_past_the_limit() {
        let dir = TempDir::new("logger_rotate");
        let path = dir.join("mula.log");
        let mut log = LogFile::new(path.clone(), 10);
        log.write_lines(&["0123456789".to_string()]).unwrap(); // 11 bytes: rotated
        assert!(!path.exists());
        assert_eq!(fs::read_to_string(dir.join("mula.log.1")).unwrap(), "0123456789\n");

        log.write_lines(&["new".to_string()]).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "new\n");
        log.write_lines(&["more text".to_string()]).unwrap(); // rotated again, replacing .1
        assert_eq!(fs::read_to_string(dir.join("mula.log.1")).unwrap(), "new\nmore text\n");
    }

    #[test]
    fn only_info_and_more_severe_records_are_enabled() {
        let logger = AppLogger { sender: channel().0 };
        let meta = |level| Metadata::builder().level(level).build();
        assert!(logger.enabled(&meta(Level::Error)));
        assert!(logger.enabled(&meta(Level::Info)));
        assert!(!logger.enabled(&meta(Level::Debug)));
    }

    #[test]
    fn log_file_lives_in_the_mula_logs_folder() {
        assert!(log_path().ends_with(Path::new("mula").join("logs").join("mula.log")));
    }
}
