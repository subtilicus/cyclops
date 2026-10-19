//! File operations: move to Recycle Bin, rename, reveal in Explorer.

use std::path::{Path, PathBuf};

/// Move `path` (file or folder) to the Recycle Bin. Where there is no Recycle Bin (e.g. some
/// network shares) Windows asks before deleting permanently.
pub fn recycle(path: &Path) -> Result<(), String> {
    imp::recycle(path)
}

/// Rename `path` within its folder to `new_name`, returning the new path.
pub fn rename(path: &Path, new_name: &str) -> Result<PathBuf, String> {
    let new_name = new_name.trim();
    if new_name.is_empty() {
        return Err("Name can't be empty".into());
    }
    if let Some(c) = new_name.chars().find(|c| r#"\/:*?"<>|"#.contains(*c) || c.is_control()) {
        return Err(format!("Names can't contain {c:?}"));
    }
    if new_name.ends_with('.') || new_name.ends_with(' ') {
        return Err("Names can't end with a dot or space".into());
    }
    let target = path.with_file_name(new_name);
    if target == path {
        return Ok(target);
    }
    // Allow case-only renames (same file on case-insensitive file systems).
    let case_only = target.to_string_lossy().eq_ignore_ascii_case(&path.to_string_lossy());
    if target.exists() && !case_only {
        return Err(format!("\"{new_name}\" already exists"));
    }
    std::fs::rename(path, &target).map_err(|e| e.to_string())?;
    Ok(target)
}

/// Open Explorer with `path` selected.
pub fn reveal(path: &Path) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer.exe")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(path)).spawn();
    }
}

#[cfg(windows)]
mod imp {
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::Win32::UI::Shell::{
        FO_DELETE, FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_SILENT, FOF_WANTNUKEWARNING, SHFILEOPSTRUCTW,
        SHFileOperationW,
    };
    use windows::core::PCWSTR;

    pub fn recycle(path: &Path) -> Result<(), String> {
        // pFrom is a list of NUL-terminated paths ending with an extra NUL.
        let mut from: Vec<u16> = path.as_os_str().encode_wide().collect();
        from.extend([0, 0]);
        let mut op = SHFILEOPSTRUCTW {
            wFunc: FO_DELETE,
            pFrom: PCWSTR(from.as_ptr()),
            fFlags: (FOF_ALLOWUNDO | FOF_NOCONFIRMATION | FOF_SILENT | FOF_WANTNUKEWARNING).0 as u16,
            ..Default::default()
        };
        let code = unsafe { SHFileOperationW(&mut op) };
        if code != 0 {
            return Err(format!("Couldn't delete (error {code:#x})"));
        }
        if op.fAnyOperationsAborted.as_bool() {
            return Err("Delete was cancelled".into());
        }
        Ok(())
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::Path;

    pub fn recycle(path: &Path) -> Result<(), String> {
        if path.is_dir() { std::fs::remove_dir_all(path) } else { std::fs::remove_file(path) }.map_err(|e| e.to_string())
    }
}
