//! External photo editor: detection of common editors, a program picker and launching.

use std::path::{Path, PathBuf};

/// Display name for an editor executable ("Photoshop", "GIMP", … or the file stem).
pub fn name(exe: &Path) -> String {
    let stem = exe.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let lower = stem.to_ascii_lowercase();
    // Photoshop versions live side by side ("Adobe Photoshop 2025", "… 2026"): keep the year.
    if lower == "photoshop" {
        if let Some(dir) = exe.parent().map(crate::fs::file_name) {
            if let Some(versioned) = dir.strip_prefix("Adobe ") {
                return versioned.to_owned();
            }
        }
    }
    let known = [
        ("photoshop", "Photoshop"),
        ("gimp", "GIMP"),
        ("krita", "Krita"),
        ("paintdotnet", "Paint.NET"),
        ("photo", "Affinity Photo"),
        ("lightroom", "Lightroom"),
        ("darktable", "darktable"),
        ("mspaint", "Paint"),
    ];
    known.iter().find(|(k, _)| lower.starts_with(k)).map_or(stem, |(_, n)| n.to_string())
}

/// Photo editors installed in their usual locations, newest versions first.
pub fn detect() -> Vec<PathBuf> {
    let mut found = Vec::new();
    let roots: Vec<PathBuf> = ["ProgramW6432", "ProgramFiles", "ProgramFiles(x86)"]
        .iter()
        .filter_map(|v| std::env::var_os(v).map(PathBuf::from))
        .collect();
    let mut add = |p: PathBuf| {
        if p.is_file() && !found.contains(&p) {
            found.push(p);
        }
    };
    // Versioned install folders: newest first.
    let versioned = |dir: &Path, prefix: &str| -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir() && crate::fs::file_name(p).starts_with(prefix))
            .collect();
        dirs.sort_by(|a, b| crate::fs::natural_cmp(&crate::fs::file_name(b), &crate::fs::file_name(a)));
        dirs
    };
    for root in &roots {
        for dir in versioned(&root.join("Adobe"), "Adobe Photoshop") {
            add(dir.join("Photoshop.exe"));
        }
        add(root.join("Affinity").join("Photo 2").join("Photo.exe"));
        add(root.join("Affinity").join("Photo").join("Photo.exe"));
        for dir in versioned(root, "GIMP") {
            for exe in ["gimp-3.exe", "gimp-3.0.exe", "gimp-2.10.exe"] {
                add(dir.join("bin").join(exe));
            }
        }
        add(root.join("Krita (x64)").join("bin").join("krita.exe"));
        add(root.join("paint.net").join("paintdotnet.exe"));
        add(root.join("darktable").join("bin").join("darktable.exe"));
    }
    found
}

/// Open `file` in `editor`, or with Windows' "Edit" action when no editor is set.
pub fn open(editor: Option<&Path>, file: &Path) -> Result<(), String> {
    match editor {
        Some(exe) => std::process::Command::new(exe)
            .arg(file)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Couldn't start {}: {e}", name(exe))),
        None => imp::shell_edit(file),
    }
}

/// Let the user pick a program (.exe). Blocks while the dialog is open.
pub fn pick_program() -> Option<PathBuf> {
    imp::pick_program()
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};

    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
    use windows::Win32::UI::Shell::Common::COMDLG_FILTERSPEC;
    use windows::Win32::UI::Shell::{FileOpenDialog, IFileOpenDialog, SIGDN_FILESYSPATH, ShellExecuteW};
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::{HSTRING, w};

    pub fn shell_edit(file: &Path) -> Result<(), String> {
        let result = unsafe { ShellExecuteW(None, w!("edit"), &HSTRING::from(file.as_os_str()), None, None, SW_SHOWNORMAL) };
        // Per the API, values above 32 mean success.
        if result.0 as isize > 32 {
            return Ok(());
        }
        // Many types (PNG, GIF, WebP…) have no "Edit" action registered; Paint opens them all.
        std::process::Command::new("mspaint.exe")
            .arg(file)
            .spawn()
            .map(|_| ())
            .map_err(|_| "Windows has no editor for this file type; choose one in Settings".into())
    }

    pub fn pick_program() -> Option<PathBuf> {
        unsafe {
            let dialog: IFileOpenDialog = CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER).ok()?;
            let filters = [COMDLG_FILTERSPEC { pszName: w!("Programs"), pszSpec: w!("*.exe") }];
            dialog.SetFileTypes(&filters).ok()?;
            dialog.SetTitle(w!("Choose a photo editor")).ok()?;
            dialog.Show(None).ok()?; // Err when cancelled
            let item = dialog.GetResult().ok()?;
            let raw = item.GetDisplayName(SIGDN_FILESYSPATH).ok()?;
            let path = raw.to_string().ok().map(PathBuf::from);
            CoTaskMemFree(Some(raw.0 as *const _));
            path
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::{Path, PathBuf};

    pub fn shell_edit(file: &Path) -> Result<(), String> {
        std::process::Command::new("xdg-open").arg(file).spawn().map(|_| ()).map_err(|e| e.to_string())
    }

    pub fn pick_program() -> Option<PathBuf> {
        None
    }
}
