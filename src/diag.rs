//! Routes `log` output (e.g. warnings from the graphics stack) into the developer log.

struct DevLogger;

impl log::Log for DevLogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Info
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            crate::perflog::record(|| format!("{:<5} {}: {}", record.level(), record.target(), record.args()));
        }
    }

    fn flush(&self) {}
}

pub fn init() {
    if log::set_logger(&DevLogger).is_ok() {
        log::set_max_level(log::LevelFilter::Info);
    }
}
