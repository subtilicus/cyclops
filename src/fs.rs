//! File system helpers: roots, directory listing and natural sorting.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::decode::{is_media, is_video};

#[derive(Clone)]
pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub is_dir: bool,
    pub is_video: bool,
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

/// Places shown at the top of the folder tree: user folders, home, drives, network.
pub fn roots() -> Vec<(String, PathBuf)> {
    let mut roots = crate::shell::known_folders();
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME")).map(PathBuf::from);
    if roots.is_empty() {
        if let Some(home) = &home {
            for sub in ["Desktop", "Downloads", "Documents", "Pictures"] {
                let p = home.join(sub);
                if p.is_dir() {
                    roots.push((sub.to_owned(), p));
                }
            }
        }
    }
    if let Some(home) = home {
        roots.push(("Home".to_owned(), home));
    }
    #[cfg(windows)]
    {
        for letter in b'A'..=b'Z' {
            let drive = PathBuf::from(format!("{}:\\", letter as char));
            if drive.exists() {
                roots.push((format!("{}:", letter as char), drive));
            }
        }
        roots.push(("Network".to_owned(), PathBuf::from(crate::shell::NETWORK)));
    }
    #[cfg(not(windows))]
    roots.push(("/".to_owned(), PathBuf::from("/")));
    roots
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Location {
    /// The virtual "Network" folder.
    Network,
    /// `\\server` — lists shares, not a real directory.
    Server,
    /// `\\server\share\`
    ShareRoot,
    /// Anything inside a share.
    Remote,
    Local,
}

pub fn location(p: &Path) -> Location {
    let s = p.to_string_lossy();
    if s == crate::shell::NETWORK {
        return Location::Network;
    }
    match s.strip_prefix(r"\\") {
        Some(rest) if !rest.starts_with(r"?\") && !rest.starts_with(r".\") => {
            match rest.split('\\').filter(|x| !x.is_empty()).count() {
                0 => Location::Local,
                1 => Location::Server,
                2 => Location::ShareRoot,
                _ => Location::Remote,
            }
        }
        _ => Location::Local,
    }
}

/// Network and server locations are listed through the shell rather than the file system.
pub fn is_virtual(p: &Path) -> bool {
    matches!(location(p), Location::Network | Location::Server)
}

/// Anything that may be slow to enumerate and must not block the UI thread.
pub fn is_remote(p: &Path) -> bool {
    location(p) != Location::Local
}

pub fn parent(p: &Path) -> Option<PathBuf> {
    match location(p) {
        Location::Network => None,
        Location::Server => Some(PathBuf::from(crate::shell::NETWORK)),
        Location::ShareRoot => {
            let s = p.to_string_lossy();
            let server = s.trim_start_matches('\\').split('\\').next().unwrap_or_default();
            Some(PathBuf::from(format!(r"\\{server}")))
        }
        Location::Remote | Location::Local => p.parent().map(Path::to_path_buf),
    }
}

pub fn display_name(p: &Path) -> String {
    let s = p.to_string_lossy();
    match location(p) {
        Location::Network => "Network".to_owned(),
        Location::Server | Location::ShareRoot => {
            s.trim_end_matches('\\').rsplit('\\').next().unwrap_or_default().to_owned()
        }
        _ => file_name(p),
    }
}

/// Subfolders of any location, including network ones. May block on the network.
pub fn subdirs_any(p: &Path) -> Vec<PathBuf> {
    if is_virtual(p) {
        crate::shell::network_children(&p.to_string_lossy()).into_iter().map(|(_, p)| p).collect()
    } else {
        subdirs(p)
    }
}

pub fn dir_entry(path: PathBuf) -> Entry {
    Entry { name: display_name(&path), path, is_dir: true, is_video: false, size: 0, modified: None, modified_label: String::new() }
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
            if is_hidden(&path, &name) || !(is_dir || is_media(&path)) {
                return None;
            }
            let modified = meta.modified().ok();
            let modified_label = modified.map(format_time).unwrap_or_default();
            let is_video = !is_dir && is_video(&path);
            Some(Entry { path, name, is_dir, is_video, size: meta.len(), modified, modified_label })
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
