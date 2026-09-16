use std::fmt::Debug;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub enum LogLevel {
    Debug = 0,
    #[default]
    Info = 1,
    Warn = 2,
    Error = 3,
}

impl LogLevel {
    pub fn as_str(&self) -> &'static str {
        match self {
            LogLevel::Debug => "DEBUG",
            LogLevel::Info => "INFO",
            LogLevel::Warn => "WARN",
            LogLevel::Error => "ERROR",
        }
    }
}

pub trait Logger: Send + Sync + Debug {
    fn log(&self, level: LogLevel, message: &str);

    fn debug(&self, message: &str) {
        self.log(LogLevel::Debug, message);
    }

    fn info(&self, message: &str) {
        self.log(LogLevel::Info, message);
    }

    fn warn(&self, message: &str) {
        self.log(LogLevel::Warn, message);
    }

    fn error(&self, message: &str) {
        self.log(LogLevel::Error, message);
    }

    fn is_enabled(&self, level: LogLevel) -> bool;
}

#[derive(Debug, Clone)]
pub struct DefaultLogger {
    level: LogLevel,
}

impl DefaultLogger {
    pub fn new() -> Self {
        Self {
            level: LogLevel::Info,
        }
    }

    pub fn with_level(level: LogLevel) -> Self {
        Self { level }
    }

    pub fn set_level(&mut self, level: LogLevel) {
        self.level = level;
    }
}

impl Default for DefaultLogger {
    fn default() -> Self {
        Self::new()
    }
}

impl Logger for DefaultLogger {
    fn log(&self, level: LogLevel, message: &str) {
        if self.is_enabled(level) {
            let timestamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
            eprintln!("[{}][{}] {}", timestamp, level.as_str(), message);
        }
    }

    fn is_enabled(&self, level: LogLevel) -> bool {
        level >= self.level
    }
}

#[derive(Debug, Clone, Default)]
pub struct NoopLogger;

impl Logger for NoopLogger {
    fn log(&self, _level: LogLevel, _message: &str) {
        // No-op
    }

    fn is_enabled(&self, _level: LogLevel) -> bool {
        false
    }
}

pub type LoggerRef = Arc<dyn Logger>;

pub fn new_logger(level: LogLevel) -> LoggerRef {
    Arc::new(DefaultLogger::with_level(level))
}

pub fn noop_logger() -> LoggerRef {
    Arc::new(NoopLogger)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Error > LogLevel::Warn);
        assert!(LogLevel::Warn > LogLevel::Info);
        assert!(LogLevel::Info > LogLevel::Debug);
    }

    #[test]
    fn test_default_logger_level() {
        let logger = DefaultLogger::new();
        assert!(logger.is_enabled(LogLevel::Info));
        assert!(logger.is_enabled(LogLevel::Warn));
        assert!(logger.is_enabled(LogLevel::Error));
        assert!(!logger.is_enabled(LogLevel::Debug));
    }

    #[test]
    fn test_default_logger_with_level() {
        let logger = DefaultLogger::with_level(LogLevel::Warn);
        assert!(!logger.is_enabled(LogLevel::Debug));
        assert!(!logger.is_enabled(LogLevel::Info));
        assert!(logger.is_enabled(LogLevel::Warn));
        assert!(logger.is_enabled(LogLevel::Error));
    }

    #[test]
    fn test_noop_logger() {
        let logger = NoopLogger;
        assert!(!logger.is_enabled(LogLevel::Error));
    }

    #[test]
    fn test_logger_convenience_methods() {
        let logger = DefaultLogger::with_level(LogLevel::Debug);
        // These should not panic
        logger.debug("debug message");
        logger.info("info message");
        logger.warn("warn message");
        logger.error("error message");
    }
}
