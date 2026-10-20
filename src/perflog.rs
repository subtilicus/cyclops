//! Developer log (decode timings, graphics setup, warnings), off by default.
//! Written to `%LOCALAPPDATA%\Cyclops\perf.log`.
//!
//! Lines logged before the setting is known (startup: GPU/surface setup) are held in memory and
//! written out once logging turns out to be enabled.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static ENABLED: AtomicBool = AtomicBool::new(false);
static DECIDED: AtomicBool = AtomicBool::new(false);
static EARLY: Mutex<Vec<String>> = Mutex::new(Vec::new());
static FILE: Mutex<Option<File>> = Mutex::new(None);

pub fn path() -> PathBuf {
    let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    base.join("Cyclops").join("perf.log")
}

fn stamp(line: &str) -> String {
    format!("{}  {line}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"))
}

fn write(line: &str) {
    let Ok(mut file) = FILE.lock() else { return };
    if file.is_none() {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        *file = OpenOptions::new().create(true).append(true).open(path).ok();
    }
    if let Some(f) = file.as_mut() {
        let _ = writeln!(f, "{line}");
    }
}

/// Turn logging on or off (from the settings). The first call flushes or drops startup lines.
pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
    if !DECIDED.swap(true, Ordering::Relaxed) {
        let early = std::mem::take(&mut *EARLY.lock().unwrap_or_else(|e| e.into_inner()));
        if on {
            for line in early {
                write(&line);
            }
        }
    }
}

/// Append a line (built lazily, only when it can be used).
pub fn record(line: impl FnOnce() -> String) {
    if ENABLED.load(Ordering::Relaxed) {
        write(&stamp(&line()));
    } else if !DECIDED.load(Ordering::Relaxed) {
        if let Ok(mut early) = EARLY.lock() {
            early.push(stamp(&line()));
        }
    }
}
