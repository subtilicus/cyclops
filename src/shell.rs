//! Windows shell integration: known folders, native icons and network enumeration.
//! Everything that can block (network, shell icon extraction) runs off the UI thread.

use std::path::PathBuf;

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::ColorImage;

/// Shell parsing name of the "Network" folder; used as a virtual path.
pub const NETWORK: &str = "::{F02C1A0D-BE21-4350-88B0-7367FC96EF3C}";

/// User folders in Explorer order, resolved through the shell so redirected
/// folders (e.g. Desktop in OneDrive) point at the right place.
pub fn known_folders() -> Vec<(String, PathBuf)> {
    imp::known_folders()
}

/// Children of a network location (the Network folder, or a `\\server`), as
/// (display name, path). Blocks; call from a background thread.
pub fn network_children(path: &str) -> Vec<(String, PathBuf)> {
    imp::network_children(path)
}

/// Explorer's thumbnail for a file (used for videos). Blocks; call from a worker thread.
pub fn thumbnail(path: &std::path::Path, px: u32) -> Option<ColorImage> {
    imp::init_com_mta();
    imp::item_image(&path.to_string_lossy(), px, false)
}

pub struct IconLoader {
    tx: Sender<(PathBuf, u32)>,
    rx: Receiver<(PathBuf, u32, Option<ColorImage>)>,
}

impl IconLoader {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, job_rx) = unbounded::<(PathBuf, u32)>();
        let (done_tx, rx) = unbounded();
        std::thread::Builder::new()
            .name("cyclops-shell-icons".into())
            .spawn(move || {
                imp::init_com();
                for (path, px) in job_rx {
                    let icon = imp::item_icon(&path.to_string_lossy(), px);
                    if done_tx.send((path, px, icon)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn icon thread");
        Self { tx, rx }
    }

    pub fn request(&self, path: PathBuf, px: u32) {
        let _ = self.tx.send((path, px));
    }

    pub fn poll(&self) -> impl Iterator<Item = (PathBuf, u32, Option<ColorImage>)> + '_ {
        self.rx.try_iter()
    }
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;

    use egui::ColorImage;
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits, GetObjectW,
        HGDIOBJ, ReleaseDC,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::Win32::System::SystemServices::SFGAO_FILESYSTEM;
    use windows::Win32::UI::Shell::*;
    use windows::core::{GUID, HSTRING, PWSTR};

    pub fn init_com() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        }
    }

    pub fn init_com_mta() {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        }
    }

    fn take_pwstr(p: PWSTR) -> String {
        unsafe {
            let s = p.to_string().unwrap_or_default();
            CoTaskMemFree(Some(p.0 as *const _));
            s
        }
    }

    pub fn known_folders() -> Vec<(String, PathBuf)> {
        let ids: [(&str, GUID); 6] = [
            ("Desktop", FOLDERID_Desktop),
            ("Downloads", FOLDERID_Downloads),
            ("Documents", FOLDERID_Documents),
            ("Pictures", FOLDERID_Pictures),
            ("Music", FOLDERID_Music),
            ("Videos", FOLDERID_Videos),
        ];
        ids.iter()
            .filter_map(|(label, id)| {
                let p = unsafe { SHGetKnownFolderPath(id, KF_FLAG_DEFAULT, None) }.ok()?;
                let path = PathBuf::from(take_pwstr(p));
                path.is_dir().then(|| (label.to_string(), path))
            })
            .collect()
    }

    pub fn network_children(path: &str) -> Vec<(String, PathBuf)> {
        init_com();
        let is_network_root = path == super::NETWORK;
        let Ok(item) = (unsafe { SHCreateItemFromParsingName::<_, _, IShellItem>(&HSTRING::from(path), None) }) else {
            return Vec::new();
        };
        let Ok(items) = (unsafe { item.BindToHandler::<_, IEnumShellItems>(None, &BHID_EnumItems) }) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        loop {
            let mut batch = [None];
            let mut fetched = 0u32;
            if unsafe { items.Next(&mut batch, Some(&mut fetched)) }.is_err() || fetched == 0 {
                break;
            }
            let Some(child) = batch[0].take() else { break };
            let Ok(parsing) = (unsafe { child.GetDisplayName(SIGDN_DESKTOPABSOLUTEPARSING) }).map(take_pwstr) else {
                continue;
            };
            let name = unsafe { child.GetDisplayName(SIGDN_NORMALDISPLAY) }.map(take_pwstr).unwrap_or_default();
            if is_network_root {
                // Keep computers (\\NAME); skip media servers, routers etc.
                let rest = parsing.strip_prefix(r"\\").unwrap_or("");
                if rest.is_empty() || rest.contains('\\') {
                    continue;
                }
                out.push((rest.to_owned(), PathBuf::from(parsing)));
            } else {
                // Keep browsable disk shares; skip printers.
                let fs = unsafe { child.GetAttributes(SFGAO_FILESYSTEM) }.is_ok_and(|a| a.contains(SFGAO_FILESYSTEM));
                if fs {
                    let share = parsing.rsplit('\\').next().unwrap_or(&name).to_owned();
                    out.push((share, PathBuf::from(format!("{parsing}\\"))));
                }
            }
        }
        out.sort_by(|a, b| crate::fs::natural_cmp(&a.0, &b.0));
        out
    }

    pub fn item_icon(path: &str, px: u32) -> Option<ColorImage> {
        item_image(path, px, true)
    }

    pub fn item_image(path: &str, px: u32, icon_only: bool) -> Option<ColorImage> {
        unsafe {
            let item: IShellItem = SHCreateItemFromParsingName(&HSTRING::from(path), None).ok()?;
            let factory: IShellItemImageFactory = windows::core::Interface::cast(&item).ok()?;
            let size = SIZE { cx: px as i32, cy: px as i32 };
            let flags = if icon_only { SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK } else { SIIGBF_BIGGERSIZEOK };
            let hbmp = factory.GetImage(size, flags).ok()?;
            let obj = HGDIOBJ(hbmp.0);
            let mut bm = BITMAP::default();
            let ok = GetObjectW(obj, size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut _)) != 0;
            let (w, h) = (bm.bmWidth, bm.bmHeight.abs());
            let mut pixels = vec![0u8; (w.max(0) * h * 4) as usize];
            let mut info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: w,
                    biHeight: -h, // top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let dc = GetDC(None);
            let lines = if ok && w > 0 {
                GetDIBits(dc, hbmp, 0, h as u32, Some(pixels.as_mut_ptr() as *mut _), &mut info, DIB_RGB_COLORS)
            } else {
                0
            };
            ReleaseDC(None, dc);
            let _ = DeleteObject(obj);
            if lines == 0 {
                return None;
            }
            // BGRA (premultiplied) -> RGBA. Legacy icons may carry no alpha at all.
            let has_alpha = pixels.chunks_exact(4).any(|p| p[3] != 0);
            for p in pixels.chunks_exact_mut(4) {
                p.swap(0, 2);
                if !has_alpha {
                    p[3] = 255;
                }
            }
            Some(ColorImage::from_rgba_premultiplied([w as usize, h as usize], &pixels))
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use std::path::PathBuf;

    use egui::ColorImage;

    pub fn init_com() {}

    pub fn known_folders() -> Vec<(String, PathBuf)> {
        Vec::new()
    }

    pub fn network_children(_path: &str) -> Vec<(String, PathBuf)> {
        Vec::new()
    }

    pub fn init_com_mta() {}

    pub fn item_icon(_path: &str, _px: u32) -> Option<ColorImage> {
        None
    }

    pub fn item_image(_path: &str, _px: u32, _icon_only: bool) -> Option<ColorImage> {
        None
    }
}
