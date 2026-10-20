//! Native window chrome: title bar colours that match the active skin, and rounded corners.

use egui::Color32;

/// Style the OS window frame. Windows 11 honours all of it; Windows 10 applies dark mode only.
pub fn style(frame: &eframe::Frame, dark: bool, caption: Color32, text: Color32, border: Color32) {
    imp::style(frame, dark, caption, text, border);
}

#[cfg(windows)]
mod imp {
    use egui::Color32;
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Dwm::*;
    use windows::core::BOOL;

    fn colorref(c: Color32) -> COLORREF {
        COLORREF(c.r() as u32 | (c.g() as u32) << 8 | (c.b() as u32) << 16)
    }

    pub fn style(frame: &eframe::Frame, dark: bool, caption: Color32, text: Color32, border: Color32) {
        let Ok(handle) = frame.window_handle() else { return };
        let RawWindowHandle::Win32(win) = handle.as_raw() else { return };
        let hwnd = HWND(win.hwnd.get() as *mut _);
        unsafe fn set<T>(hwnd: HWND, attribute: DWMWINDOWATTRIBUTE, value: &T) {
            // Older Windows versions reject unknown attributes; that's fine.
            let _ = unsafe { DwmSetWindowAttribute(hwnd, attribute, value as *const T as *const _, size_of::<T>() as u32) };
        }
        unsafe {
            set(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, &BOOL::from(dark));
            set(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE, &DWMWCP_ROUND);
            set(hwnd, DWMWA_CAPTION_COLOR, &colorref(caption));
            set(hwnd, DWMWA_TEXT_COLOR, &colorref(text));
            set(hwnd, DWMWA_BORDER_COLOR, &colorref(border));
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use egui::Color32;

    pub fn style(_frame: &eframe::Frame, _dark: bool, _caption: Color32, _text: Color32, _border: Color32) {}
}
