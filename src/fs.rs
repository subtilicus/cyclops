//! File system helpers: roots, directory listing and natural sorting.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::decode::is_image;

#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified: Option<SystemTime>,
    /// Pre-formatted local modification time, so tiles don't format every frame.
    pub modified_label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Modified,
    Size,
    Type,
}

impl SortKey {
    pub const ALL: [Self; 4] = [Self::Name, Self::Modified, Self::Size, Self::Type];

    pub fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Modified => "Date modified",
            Self::Size => "Size",
            Self::Type => "Type",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|k| k.label() == label)
    }
}

/// Places shown at the top of the folder tree.
pub fn roots() -> Vec<(String, PathBuf)> {
    let mut roots = Vec::new();
    if let Some(home) = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")) {
        let home = PathBuf::from(home);
        for (label, sub) in [("Pictures", "Pictures"), ("Desktop", "Desktop"), ("Downloads", "Downloads")] {
            let p = home.join(sub);
            if p.is_dir() {
                roots.push((label.to_owned(), p));
            }
        }
        roots.push(("Home".to_owned(), home));
    }
    #[cfg(windows)]
    for letter in b'A'..=b'Z' {
        let drive = PathBuf::from(format!("{}:\\", letter as char));
        if drive.exists() {
            roots.push((format!("{}:", letter as char), drive));
        }
    }
    #[cfg(not(windows))]
    roots.push(("/".to_owned(), PathBuf::from("/")));
    roots
}

fn is_hidden(path: &Path, name: &str) -> bool {
    if name.starts_with('.') || name.starts_with('$') {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const HIDDEN: u32 = 0x2;
        const SYSTEM: u32 = 0x4;
        if let Ok(m) = std::fs::symlink_metadata(path) {
            return m.file_attributes() & (HIDDEN | SYSTEM) != 0;
        }
    }
    let _ = path;
    false
}

/// Visible subdirectories of `dir`, naturally sorted.
pub fn subdirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = rd
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| !is_hidden(p, &file_name(p)))
        .collect();
    dirs.sort_by(|a, b| natural_cmp(&file_name(a), &file_name(b)));
    dirs
}

/// Folders first, then images, each naturally sorted.
pub fn list(dir: &Path) -> std::io::Result<Vec<Entry>> {
    let mut entries: Vec<Entry> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            let is_dir = meta.is_dir();
            if is_hidden(&path, &name) || !(is_dir || is_image(&path)) {
                return None;
            }
            let modified = meta.modified().ok();
            let modified_label = modified.map(format_time).unwrap_or_default();
            Some(Entry { path, name, is_dir, size: meta.len(), modified, modified_label })
        })
        .collect();
    sort(&mut entries, SortKey::Name, false);
    Ok(entries)
}

/// Folders always come first; ties fall back to natural name order.
pub fn sort(entries: &mut [Entry], key: SortKey, descending: bool) {
    let ext = |e: &Entry| e.path.extension().map(|x| x.to_string_lossy().to_lowercase()).unwrap_or_default();
    entries.sort_by(|a, b| {
        let ord = match key {
            SortKey::Name => Ordering::Equal,
            SortKey::Modified => a.modified.cmp(&b.modified),
            SortKey::Size => a.size.cmp(&b.size),
            SortKey::Type => ext(a).cmp(&ext(b)),
        }
        .then_with(|| natural_cmp(&a.name, &b.name));
        b.is_dir.cmp(&a.is_dir).then(if descending { ord.reverse() } else { ord })
    });
}

pub fn format_time(t: SystemTime) -> String {
    chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string()
}

pub fn file_name(p: &Path) -> String {
    p.file_name().map_or_else(|| p.display().to_string(), |n| n.to_string_lossy().into_owned())
}

/// Case-insensitive comparison that orders embedded numbers numerically ("img2" < "img10").
pub fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut a, mut b) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (a.peek().copied(), b.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let take_num = |it: &mut std::iter::Peekable<std::str::Chars>| {
                    let mut s = String::new();
                    while let Some(c) = it.peek().copied().filter(char::is_ascii_digit) {
                        s.push(c);
                        it.next();
                    }
                    s
                };
                let (na, nb) = (take_num(&mut a), take_num(&mut b));
                let (ta, tb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                a.next();
                b.next();
            }
        }
    }
}

pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut unit = 0;
    while v >= 1024.0 && unit < UNITS.len() - 1 {
        v /= 1024.0;
        unit += 1;
    }
    if unit == 0 { format!("{bytes} B") } else { format!("{v:.1} {}", UNITS[unit]) }
}
