//! Optional developer performance log (decode timings), off by default.
//! Written to `%LOCALAPPDATA%\Cyclops\perf.log`.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub static ENABLED: AtomicBool = AtomicBool::new(false);

pub fn path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Cyclops").join("perf.log")
}

/// Append a line (built lazily, only when logging is on).
pub fn record(line: impl FnOnce() -> String) {
    if !ENABLED.load(Ordering::Relaxed) {
        return;
    }
    static FILE: Mutex<Option<File>> = Mutex::new(None);
    let Ok(mut file) = FILE.lock() else { return };
    if file.is_none() {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        *file = OpenOptions::new().create(true).append(true).open(path).ok();
    }
    if let Some(f) = file.as_mut() {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
        let _ = writeln!(f, "{now}  {}", line());
    }
}
