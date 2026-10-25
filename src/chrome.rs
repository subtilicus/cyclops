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

/// Where Windows' caption buttons run into our content: a maximized window loses its top frame
/// off-screen and its title bar shrinks by that much, but the buttons (and the red close hover)
/// keep their height. Returns the overlap's height and the buttons' left edge, in physical
/// pixels from the client area's top-left; `None` when nothing overlaps.
pub fn caption_overlap(frame: &eframe::Frame) -> Option<(f32, f32)> {
    imp::caption_overlap(frame)
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

    fn hwnd(frame: &eframe::Frame) -> Option<HWND> {
        let handle = frame.window_handle().ok()?;
        let RawWindowHandle::Win32(win) = handle.as_raw() else { return None };
        Some(HWND(win.hwnd.get() as *mut _))
    }

    pub fn caption_overlap(frame: &eframe::Frame) -> Option<(f32, f32)> {
        use windows::Win32::Foundation::{POINT, RECT};
        use windows::Win32::Graphics::Gdi::ClientToScreen;
        use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
        use windows::Win32::UI::WindowsAndMessaging::*;
        let hwnd = hwnd(frame)?;
        unsafe {
            let captioned = (GetWindowLongPtrW(hwnd, GWL_STYLE) as u32 & WS_CAPTION.0) == WS_CAPTION.0;
            if !captioned || !IsZoomed(hwnd).as_bool() {
                return None;
            }
            let mut window = RECT::default();
            GetWindowRect(hwnd, &mut window).ok()?;
            let mut origin = POINT::default();
            ClientToScreen(hwnd, &mut origin).ok().ok()?;
            let mut buttons = RECT::default();
            DwmGetWindowAttribute(
                hwnd,
                DWMWA_CAPTION_BUTTON_BOUNDS,
                &mut buttons as *mut RECT as *mut _,
                size_of::<RECT>() as u32,
            )
            .ok()?;
            // The buttons keep roughly the height of the frame that went off-screen.
            let dpi = GetDpiForWindow(hwnd).max(96);
            let hidden = GetSystemMetricsForDpi(SM_CYFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi);
            Some((hidden as f32, (window.left + buttons.left - origin.x) as f32))
        }
    }

    pub fn style(frame: &eframe::Frame, dark: bool, material: Material, caption: Color32, text: Color32, border: Color32) {
        let Some(hwnd) = hwnd(frame) else { return };
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
        static ICON: std::sync::Once = std::sync::Once::new();
        ICON.call_once(|| set_icon(hwnd));
    }

    /// Give the window the exe's embedded icon (its hand-tuned 16-32 px frames, at the window's
    /// DPI) and redraw the frame: an icon set after the caption is first drawn otherwise doesn't
    /// show until something else repaints it.
    fn set_icon(hwnd: HWND) {
        use windows::Win32::Foundation::{HINSTANCE, LPARAM, WPARAM};
        use windows::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
        use windows::Win32::UI::WindowsAndMessaging::*;
        use windows::core::PCWSTR;
        unsafe {
            let module = HINSTANCE(GetWindowLongPtrW(hwnd, GWLP_HINSTANCE) as *mut _);
            let dpi = GetDpiForWindow(hwnd).max(96);
            for (kind, metric) in [(ICON_SMALL, SM_CXSMICON), (ICON_BIG, SM_CXICON)] {
                let side = GetSystemMetricsForDpi(metric, dpi);
                // Resource 1 is the icon build.rs embeds.
                if let Ok(icon) = LoadImageW(Some(module), PCWSTR(1 as _), IMAGE_ICON, side, side, LR_DEFAULTCOLOR) {
                    SendMessageW(hwnd, WM_SETICON, Some(WPARAM(kind as usize)), Some(LPARAM(icon.0 as isize)));
                }
            }
            let _ = SetWindowPos(
                hwnd,
                None,
                0,
                0,
                0,
                0,
                SWP_FRAMECHANGED | SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }
}

#[cfg(not(windows))]
mod imp {
    use egui::Color32;

    use super::Material;

    pub fn style(_frame: &eframe::Frame, _dark: bool, _material: Material, _caption: Color32, _text: Color32, _border: Color32) {}

    pub fn caption_overlap(_frame: &eframe::Frame) -> Option<(f32, f32)> {
        None
    }
}
