//! Native window chrome: title bar colours that match the active skin, rounded corners, and the
//! Windows 11 system backdrop (Mica / Acrylic) behind the translucent UI.

use egui::Color32;

/// What shows behind the window's translucent panels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Material {
    /// Opaque panels, no system backdrop.
    Solid,
    /// Windows 11 Mica: the desktop wallpaper, heavily blurred and tinted.
    Mica,
    /// Windows 11 Acrylic: a frosted view of whatever is behind the window.
    Acrylic,
}

impl Material {
    pub const ALL: [Self; 3] = [Self::Solid, Self::Mica, Self::Acrylic];

    pub fn label(self) -> &'static str {
        match self {
            Self::Solid => "Solid",
            Self::Mica => "Mica",
            Self::Acrylic => "Acrylic",
        }
    }

    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.label() == label)
    }

    pub fn translucent(self) -> bool {
        self != Self::Solid
    }
}

/// Style the OS window frame. Windows 11 honours all of it; Windows 10 applies dark mode only.
pub fn style(frame: &eframe::Frame, dark: bool, material: Material, caption: Color32, text: Color32, border: Color32) {
    imp::style(frame, dark, material, caption, text, border);
}

#[cfg(windows)]
mod imp {
    use egui::Color32;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::*;
    use windows::Win32::UI::Controls::MARGINS;
    use windows::core::BOOL;

    use super::Material;

    fn colorref(c: Color32) -> COLORREF {
        COLORREF(c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16)
    }

    pub fn style(frame: &eframe::Frame, dark: bool, material: Material, caption: Color32, text: Color32, border: Color32) {
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(win) = handle.as_raw() else { return };
        let hwnd = HWND(win.hwnd.get() as *mut _);
        unsafe fn set<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) {
            // Older Windows versions reject unknown attributes; that's fine.
            let _ = unsafe { DwmSetWindowAttribute(hwnd, attribute, value as *const T as *const _, size_of::<T>() as u32) };
        }
        let backdrop = match material {
            Material::Solid => DWMSBT_NONE,
            Material::Mica => DWMSBT_MAINWINDOW,
            Material::Acrylic => DWMSBT_TRANSIENTWINDOW,
        };
        unsafe {
            set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(dark));
            set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
            set(hwnd, DWMWA_SYSTEMBACKDROP_TYPE, &backdrop);
            // The backdrop must reach under the whole client area, which is drawn translucent.
            let extend = if material.translucent() { -1 } else { 0 };
            let margins = MARGINS { cxLeftWidth: extend, cxRightWidth: extend, cyTopHeight: extend, cyBottomHeight: extend };
            let _ = DwmExtendFrameIntoClientArea(hwnd, &margins);
            // The GDI surface under our GPU layer must be black: inside the extended frame, black
            // means "show the system backdrop". (Otherwise it is erased white and hides Mica.)
            if material.translucent() {
                use windows::Win32::Graphics::Gdi::{BLACK_BRUSH, GetStockObject, RDW_ERASE, RDW_FRAME, RDW_INVALIDATE, RedrawWindow};
                use windows::Win32::UI::WindowsAndMessaging::{GCLP_HBRBACKGROUND, SetClassLongPtrW};
                SetClassLongPtrW(hwnd, GCLP_HBRBACKGROUND, GetStockObject(BLACK_BRUSH).0 as isize);
                let _ = RedrawWindow(Some(hwnd), None, None, RDW_ERASE | RDW_INVALIDATE | RDW_FRAME);
            }
            // Always our own caption colour: "default" would pick up the user's accent colour
            // when Windows is set to show it on title bars.
            set(hwnd, DWMWA_CAPTION_COLOR, &colorref(caption));
            set(hwnd, DWMWA_TEXT_COLOR, &colorref(text));
            set(hwnd, DWMWA_BORDER_COLOR, &colorref(border));
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use egui::Color32;

    use super::Material;

    pub fn style(_frame: &eframe::Frame, _dark: bool, _material: Material, _caption: Color32, _text: Color32, _border: Color32) {}
}
