use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};

use crossbeam_channel::{Receiver, Sender, unbounded};
use egui::text::{LayoutJob, TextWrapping};
use egui::{
    Align2, Color32, CornerRadius, FontId, Key, Rect, Sense, Stroke, TextFormat, TextureHandle, TextureOptions,
    Vec2, pos2, vec2,
};

use crate::fs::{self, Entry, SortKey};
use crate::icons::{self, Icon};
use crate::loader::{Kind, Loader};
use crate::ops;
use crate::shell::{self, IconLoader};
use crate::theme;
use crate::video::{self, VideoPlayer};

/// Thumbnails are decoded at this size and scaled on the GPU for display.
const THUMB_DECODE_SIDE: u32 = 320;
const MAX_LIVE_THUMBS: usize = 3000;
const FULL_CACHE: usize = 8;
const PREFETCH: usize = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Skin {
    System,
    Dark,
    Light,
}

impl Skin {
    const ALL: [Self; 3] = [Self::System, Self::Dark, Self::Light];

    fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Dark => "Dark",
            Self::Light => "Light",
        }
    }

    fn preference(self) -> egui::ThemePreference {
        match self {
            Self::System => egui::ThemePreference::System,
            Self::Dark => egui::ThemePreference::Dark,
            Self::Light => egui::ThemePreference::Light,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SettingsPage {
    Appearance,
    Browsing,
    Viewer,
    Editor,
    Developer,
}

impl SettingsPage {
    const ALL: [Self; 5] = [Self::Appearance, Self::Browsing, Self::Viewer, Self::Editor, Self::Developer];

    fn label(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Browsing => "Browsing",
            Self::Viewer => "Viewer",
            Self::Editor => "Editor",
            Self::Developer => "Developer",
        }
    }
}

struct Settings {
    skin: Skin,
    /// Viewer mouse wheel zooms (true) or flips between images (false).
    wheel_zooms: bool,
    show_dates: bool,
    show_info_bar: bool,
    /// Opening an image from the browser goes fullscreen.
    open_fullscreen: bool,
    /// Fit enlarges images smaller than the view (otherwise they stop at 100%).
    enlarge_small: bool,
    video_muted: bool,
    video_volume: f32,
    /// Folders | thumbnails | preview, instead of folders | thumbnails.
    three_pane: bool,
    /// Developer: write decode timings to a log file.
    perf_log: bool,
    /// External photo editor; `None` uses Windows' "Edit" action.
    editor: Option<PathBuf>,
    /// Window material behind the translucent chrome.
    material: crate::chrome::Material,
    /// Width of the preview pane in points.
    preview_width: f32,
}

enum Thumb {
    Pending,
    Ready { tex: TextureHandle, last_used: u64 },
    Failed,
}

enum Full {
    Pending,
    Ready(FullImage),
    Failed(String),
}

struct FullImage {
    main: TextureHandle,
    screen: Option<TextureHandle>,
    source_size: Vec2,
    /// Animation frames with durations in seconds (empty for stills).
    frames: Vec<(TextureHandle, f32)>,
    /// When the image became ready; animations play from here.
    ready_at: f64,
    /// Animation position (seconds) while paused; `None` while playing.
    paused_at: Option<f32>,
    /// HDR/EXR linear data and the exposure (EV) being shown.
    hdr: Option<HdrEdit>,
    /// Tiny blurred copy for frosted-glass overlays.
    blur: Option<TextureHandle>,
    bit_depth: Option<crate::decode::BitDepth>,
    /// Storage format worth showing (e.g. "BC7" for GPU textures).
    format: Option<String>,
    /// GPU textures: [levels, layers, faces], every sub-image, and which one is shown.
    sub_layout: Option<[u32; 3]>,
    subs: Vec<SubTex>,
    sub_current: usize,
}

/// One mip level / layer / face of a GPU texture, ready to display.
struct SubTex {
    coords: [u32; 3],
    main: TextureHandle,
    /// HDR data of this sub-image while it is *not* shown (the shown one lives in `FullImage`).
    hdr: Option<HdrEdit>,
    size: Vec2,
}

const CUBE_FACES: [&str; 6] = ["+X", "−X", "+Y", "−Y", "+Z", "−Z"];

impl FullImage {
    /// Show sub-image `index` (swapping the current one's HDR data back into its slot).
    fn select_sub(&mut self, index: usize) {
        if index == self.sub_current || index >= self.subs.len() {
            return;
        }
        std::mem::swap(&mut self.hdr, &mut self.subs[self.sub_current].hdr);
        std::mem::swap(&mut self.hdr, &mut self.subs[index].hdr);
        let sub = &self.subs[index];
        self.main = sub.main.clone();
        self.source_size = sub.size;
        self.screen = None;
        self.sub_current = index;
    }

    /// Index of the sub-image at `coords`, if the texture has one.
    fn find_sub(&self, coords: [u32; 3]) -> Option<usize> {
        self.subs.iter().position(|s| s.coords == coords)
    }
}

struct HdrEdit {
    gpu: crate::hdr_gpu::HdrTexture,
    auto_exposure: f32,
    /// ACES tone mapping (scene-referred) vs plain clipping (display-referred).
    tonemap: bool,
    /// Stops relative to auto exposure.
    ev: f32,
}

impl HdrEdit {
    fn exposure(&self) -> f32 {
        self.auto_exposure * 2f32.powf(self.ev)
    }
}

/// A file command picked from a menu or shortcut; applied after the frame's UI is drawn.
enum FileAction {
    Open(usize),
    Reveal(PathBuf),
    Rename(usize),
    Delete(usize),
    /// Create a folder inside this one and start renaming it.
    NewFolder(PathBuf),
    /// Open in the external editor.
    Edit(usize),
}

enum Dialog {
    Delete { path: PathBuf, name: String, is_dir: bool },
    Rename { path: PathBuf, text: String, error: Option<String>, fresh: bool },
}

impl FullImage {
    /// The animation frame to show at `now` (scheduling a repaint for the next one),
    /// or `None` for still images.
    fn frame_at(&self, now: f64, ctx: &egui::Context) -> Option<egui::TextureId> {
        let (index, remaining) = self.frame_index(now)?;
        if self.paused_at.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_secs_f32(remaining));
        }
        Some(self.frames[index].0.id())
    }

    fn anim_duration(&self) -> f32 {
        self.frames.iter().map(|(_, d)| d).sum()
    }

    /// Seconds into the animation loop.
    fn anim_time(&self, now: f64) -> f32 {
        let total = self.anim_duration().max(1e-3);
        self.paused_at.unwrap_or_else(|| ((now - self.ready_at) as f32).rem_euclid(total))
    }

    /// Current frame and the time left until the next one.
    fn frame_index(&self, now: f64) -> Option<(usize, f32)> {
        if self.anim_duration() <= 0.0 {
            return None;
        }
        let mut t = self.anim_time(now);
        for (i, (_, duration)) in self.frames.iter().enumerate() {
            if t < *duration {
                return Some((i, duration - t));
            }
            t -= duration;
        }
        Some((self.frames.len() - 1, 0.0))
    }

    fn toggle_animation(&mut self, now: f64) {
        match self.paused_at.take() {
            Some(t) => self.ready_at = now - t as f64,
            None => self.paused_at = Some(self.anim_time(now)),
        }
    }

    fn seek_animation(&mut self, now: f64, t: f32) {
        let t = t.clamp(0.0, (self.anim_duration() - 1e-3).max(0.0));
        if self.paused_at.is_some() {
            self.paused_at = Some(t);
        } else {
            self.ready_at = now - t as f64;
        }
    }
}

/// A timed zoom from one scale to another, keeping `focus` fixed on screen.
#[derive(Clone, Copy)]
struct ZoomAnim {
    from_scale: f32,
    to_scale: f32,
    from_pan: Vec2,
    focus: egui::Pos2,
    start: f64,
    duration: f32,
    /// Retargeted mid-flight (e.g. continued wheel scrolling): ease out only, so the
    /// motion keeps its momentum instead of stalling at the start of a new S-curve.
    chained: bool,
    /// Ends in "fit" mode, gliding the image back to the centre on the way.
    to_fit: bool,
}

impl ZoomAnim {
    fn eased(&self, now: f64) -> (f32, bool) {
        let u = (((now - self.start) as f32) / self.duration).clamp(0.0, 1.0);
        let e = if self.chained {
            1.0 - (1.0 - u).powi(3) // ease-out cubic
        } else {
            u * u * u * (u * (u * 6.0 - 15.0) + 10.0) // smootherstep: gentle S-curve
        };
        (e, u >= 1.0)
    }
}

/// Zoom and pan of an image inside a view (the fullscreen viewer and the preview pane).
#[derive(Default)]
struct ZoomPan {
    /// Points per source pixel; `None` means fit to the view.
    zoom: Option<f32>,
    pan: Vec2,
    /// Running zoom animation, if any.
    anim: Option<ZoomAnim>,
}

impl ZoomPan {
    /// Apply this frame's input and return where to draw the image and at what scale.
    ///
    /// `factor` is the wheel/pinch/keyboard zoom (anchored at `focus`); double-click toggles
    /// between fit and 100%, dragging pans. `can_zoom` is false while only a placeholder is shown.
    #[allow(clippy::too_many_arguments)]
    fn update(
        &mut self,
        ctx: &egui::Context,
        rect: Rect,
        resp: &egui::Response,
        natural: Vec2,
        fit: f32,
        factor: f32,
        focus: egui::Pos2,
        can_zoom: bool,
    ) -> (Rect, f32) {
        let ppp = ctx.pixels_per_point();
        let scale = self.zoom.unwrap_or(fit);
        let (min_scale, max_scale) = (fit * 0.5, 32.0 / ppp);
        let now = ctx.input(|i| i.time);
        let start_zoom = |zp: &mut ZoomPan, to: f32, focus: egui::Pos2, to_fit: bool| {
            let chained = zp.anim.is_some();
            zp.anim = Some(ZoomAnim {
                from_scale: scale,
                to_scale: to,
                from_pan: zp.pan,
                focus,
                start: now,
                duration: if chained { 0.22 } else { 0.32 },
                chained,
                to_fit,
            });
        };
        if (factor - 1.0).abs() > 1e-4 && can_zoom {
            let base = self.anim.map_or(scale, |a| a.to_scale);
            start_zoom(self, (base * factor).clamp(min_scale, max_scale), focus, false);
        }
        if resp.double_clicked() && can_zoom {
            let focus = resp.interact_pointer_pos().unwrap_or(rect.center());
            if self.zoom.is_some() && (scale - fit).abs() > 1e-4 {
                start_zoom(self, fit, focus, true);
            } else {
                start_zoom(self, (1.0 / ppp).max(fit * 2.0).min(max_scale), focus, false);
            }
        }
        if resp.drag_started() {
            self.anim = None;
        }

        if let Some(a) = self.anim {
            let (e, done) = a.eased(now);
            // Interpolate in log space so zooming in and out feel equally fast.
            let new_scale = a.from_scale * (a.to_scale / a.from_scale).powf(e);
            let from_center = rect.center() + a.from_pan;
            let new_center = a.focus - (a.focus - from_center) * (new_scale / a.from_scale);
            self.pan = new_center - rect.center();
            if a.to_fit {
                self.pan *= 1.0 - e;
            }
            self.zoom = Some(new_scale);
            if done {
                self.anim = None;
                if a.to_fit {
                    self.zoom = None;
                    self.pan = Vec2::ZERO;
                }
            } else {
                ctx.request_repaint();
            }
        }

        if resp.dragged() {
            self.pan += resp.drag_delta();
        }

        let scale = self.zoom.unwrap_or(fit);
        let size = natural * scale;
        // Allow panning until an image edge reaches the view centre, so any part of the
        // image (corners included) can be centred, but it can never be lost entirely.
        let max_pan = size / 2.0;
        self.pan = self.pan.clamp(-max_pan, max_pan);
        if resp.hovered() {
            ctx.set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
        }
        (Rect::from_center_size(rect.center() + self.pan, size), scale)
    }
}

/// This frame's wheel travel over a hovered view, split into (plain, with Ctrl), plus pinch zoom.
fn wheel_input(ctx: &egui::Context, hovered: bool) -> (f32, f32, f32) {
    if !hovered {
        return (0.0, 0.0, 1.0);
    }
    ctx.input(|i| {
        i.events.iter().fold((0.0f32, 0.0f32, 1.0f32), |(w, c, z), e| match e {
            egui::Event::MouseWheel { unit, delta, modifiers, .. } => {
                let d = match unit {
                    egui::MouseWheelUnit::Line => delta.y * 50.0,
                    egui::MouseWheelUnit::Page => delta.y * 500.0,
                    egui::MouseWheelUnit::Point => delta.y,
                };
                if modifiers.ctrl { (w, c + d, z) } else { (w + d, c, z) }
            }
            egui::Event::Zoom(f) => (w, c, z * f),
            _ => (w, c, z),
        })
    })
}

fn wheel_zoom_factor(wheel: f32) -> f32 {
    (wheel * 0.0035).exp()
}

struct Viewer {
    /// Index into `entries` (always an image).
    index: usize,
    view: ZoomPan,
    /// Wheel travel accumulated towards the next image (when the wheel navigates).
    wheel_accum: f32,
}

pub struct CyclopsApp {
    loader: Loader,
    ctx: egui::Context,
    /// eframe's wgpu state, for custom GPU drawing (HDR tone mapping).
    render_state: Option<eframe::egui_wgpu::RenderState>,
    settings: Settings,
    roots: Vec<(String, PathBuf)>,
    tree_children: HashMap<PathBuf, Vec<PathBuf>>,
    /// Remote folders whose children are being enumerated on a background thread.
    children_loading: HashSet<PathBuf>,
    children_tx: Sender<(PathBuf, Vec<PathBuf>)>,
    children_rx: Receiver<(PathBuf, Vec<PathBuf>)>,
    /// Whether tree folders have subfolders (for showing the expand chevron), probed in the
    /// background so big folders never stall the UI.
    has_subdirs: HashMap<PathBuf, bool>,
    probe_tx: Sender<PathBuf>,
    probe_rx: Receiver<(PathBuf, bool)>,
    /// Native shell icons for special folders, drives and network locations (None = pending/failed).
    icon_loader: IconLoader,
    shell_icons: HashMap<(PathBuf, u32), Option<TextureHandle>>,
    special: HashSet<PathBuf>,
    /// Servers and share roots the user has visited (persisted).
    network_places: Vec<PathBuf>,
    /// Expanded tree nodes, keyed by (top-level root, path): the same folder can appear under
    /// several roots (e.g. Pictures and C:\Users\…\Pictures) and each branch is independent.
    expanded: HashSet<(PathBuf, PathBuf)>,
    /// The top-level tree root the current folder is being browsed under.
    tree_root: Option<PathBuf>,
    scroll_tree_to_folder: bool,

    folder: Option<PathBuf>,
    path_edit: String,
    entries: Vec<Entry>,
    list_error: Option<String>,
    selected: Option<usize>,
    history: Vec<PathBuf>,
    sort_key: SortKey,
    sort_desc: bool,

    thumb_size: f32,
    thumbs: HashMap<PathBuf, Thumb>,
    grid_cols: usize,
    grid_viewport: Rect,
    scroll_to_selected: bool,

    viewer: Option<Viewer>,
    full: HashMap<PathBuf, Full>,
    full_lru: VecDeque<PathBuf>,
    fullscreen: bool,
    /// Fullscreen was switched on by opening the viewer (and is undone when it closes).
    fullscreen_for_viewer: bool,
    pending_key_zoom: f32,
    pending_action: Option<FileAction>,
    /// What's behind glass overlays this frame (set when the viewer/preview draws its image).
    backdrop: Option<crate::glass::Backdrop>,
    /// The settings window, if open, and the page it shows.
    settings_page: Option<SettingsPage>,
    /// Photo editors found on this PC (for the settings menu).
    detected_editors: Vec<PathBuf>,
    /// Type-to-find in the grid: the prefix typed so far and when the last key came in.
    typeahead: (String, f64),
    /// Window focus last frame; regaining focus re-syncs the folder (e.g. after editing).
    was_focused: bool,
    /// Whether the restored window geometry has been checked against the current screen.
    window_checked: bool,
    /// Skin and material the native window was last styled for.
    chrome_state: Option<(bool, crate::chrome::Material)>,
    dialog: Option<Dialog>,
    /// A short message (e.g. a failed delete) and when it was posted.
    toast: Option<(String, f64)>,
    /// Zoom/pan of the preview pane, and the file it applies to (reset on selection change).
    preview_view: ZoomPan,
    preview_for: Option<PathBuf>,
    /// The video playing in the viewer (path it was opened for, player or error).
    video: Option<(PathBuf, Result<VideoPlayer, String>)>,
    /// Last time the pointer moved or clicked in the viewer (drives control auto-hide).
    last_pointer_activity: f64,

    frame_no: u64,
    max_texture_side: u32,
    screen_side: u32,
}

impl CyclopsApp {
    pub fn new(cc: &eframe::CreationContext<'_>, start: Option<PathBuf>) -> Self {
        let stored = |key: &str| cc.storage.and_then(|s| s.get_string(key));
        let flag = |key: &str, default: bool| stored(key).map_or(default, |s| s == "true");
        let settings = Settings {
            skin: stored("skin").and_then(|s| Skin::ALL.into_iter().find(|k| k.label() == s)).unwrap_or(Skin::Dark),
            wheel_zooms: flag("wheel_zooms", true),
            show_dates: flag("show_dates", true),
            show_info_bar: flag("show_info_bar", true),
            open_fullscreen: flag("open_fullscreen", true),
            enlarge_small: flag("enlarge_small", true),
            video_muted: flag("video_muted", false),
            video_volume: stored("video_volume").and_then(|s| s.parse().ok()).unwrap_or(1.0),
            three_pane: flag("three_pane", false),
            perf_log: flag("perf_log", false),
            editor: stored("editor").filter(|s| !s.is_empty()).map(PathBuf::from),
            material: stored("material")
                .and_then(|s| crate::chrome::Material::from_label(&s))
                .unwrap_or(crate::chrome::Material::Mica),
            preview_width: stored("preview_width").and_then(|s| s.parse().ok()).unwrap_or(460.0),
        };
        theme::install(&cc.egui_ctx);
        if let Some(rs) = &cc.wgpu_render_state {
            crate::hdr_gpu::init(rs);
            let info = rs.adapter.get_info();
            log::info!("adapter: {} ({:?}, {:?}), target format {:?}", info.name, info.backend, info.device_type, rs.target_format);
        }
        cc.egui_ctx.set_theme(settings.skin.preference());
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let thumb_size = stored("thumb_size").and_then(|s| s.parse().ok()).unwrap_or(160.0);
        let sort_key = stored("sort_key").and_then(|s| SortKey::from_label(&s)).unwrap_or(SortKey::Name);
        let sort_desc = stored("sort_desc").is_some_and(|s| s == "true");
        let last_folder = stored("last_folder").map(PathBuf::from).filter(|p| p.is_dir() || fs::is_virtual(p));

        let roots = fs::roots();
        let special = roots.iter().map(|(_, p)| p.clone()).collect();
        let (children_tx, children_rx) = unbounded();
        let (probe_tx, probe_jobs) = unbounded::<PathBuf>();
        let (probe_done, probe_rx) = unbounded();
        {
            let ctx = cc.egui_ctx.clone();
            std::thread::Builder::new()
                .name("cyclops-tree-probe".into())
                .spawn(move || {
                    for path in probe_jobs {
                        let has = fs::has_subdirs(&path);
                        if probe_done.send((path, has)).is_err() {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawn tree probe thread");
        }
        let mut app = Self {
            loader: Loader::new(cc.egui_ctx.clone()),
            ctx: cc.egui_ctx.clone(),
            render_state: cc.wgpu_render_state.clone(),
            settings,
            roots,
            tree_children: HashMap::new(),
            children_loading: HashSet::new(),
            children_tx,
            children_rx,
            has_subdirs: HashMap::new(),
            probe_tx,
            probe_rx,
            icon_loader: IconLoader::new(cc.egui_ctx.clone()),
            shell_icons: HashMap::new(),
            special,
            network_places: stored("network_places")
                .map(|s| s.lines().filter(|l| !l.is_empty()).map(PathBuf::from).collect())
                .unwrap_or_default(),
            expanded: HashSet::new(),
            tree_root: None,
            scroll_tree_to_folder: true,
            folder: None,
            path_edit: String::new(),
            entries: Vec::new(),
            list_error: None,
            selected: None,
            history: Vec::new(),
            sort_key,
            sort_desc,
            thumb_size,
            thumbs: HashMap::new(),
            grid_cols: 1,
            grid_viewport: Rect::NOTHING,
            scroll_to_selected: false,
            viewer: None,
            full: HashMap::new(),
            full_lru: VecDeque::new(),
            fullscreen: false,
            fullscreen_for_viewer: false,
            pending_key_zoom: 1.0,
            video: None,
            pending_action: None,
            backdrop: None,
            settings_page: None,
            detected_editors: crate::editor::detect(),
            typeahead: (String::new(), 0.0),
            was_focused: true,
            chrome_state: None,
            window_checked: false,
            dialog: None,
            toast: None,
            preview_view: ZoomPan::default(),
            preview_for: None,
            last_pointer_activity: 0.0,
            frame_no: 0,
            max_texture_side: 8192,
            screen_side: 2560,
        };

        match start {
            Some(p) if p.is_file() => {
                if let Some(parent) = p.parent() {
                    app.navigate(parent.to_path_buf());
                    if let Some(i) = app.entries.iter().position(|e| e.path == p) {
                        app.selected = Some(i);
                        app.open_viewer(i);
                    }
                }
            }
            Some(p) if p.is_dir() || fs::is_virtual(&p) => app.navigate(p),
            _ => {
                let pictures = app.roots.iter().find(|(l, _)| l == "Pictures");
                let fallback = pictures.or(app.roots.first()).map(|(_, p)| p.clone());
                if let Some(p) = last_folder.or(fallback) {
                    app.navigate(p);
                }
            }
        }
        app
    }

    // ------------------------------------------------------------------ navigation

    fn navigate(&mut self, folder: PathBuf) {
        if self.folder.as_ref() == Some(&folder) {
            return;
        }
        if let Some(prev) = self.folder.take() {
            self.history.push(prev);
        }
        self.load_folder(folder);
    }

    fn load_folder(&mut self, folder: PathBuf) {
        // Share roots always carry a trailing separator, matching what `Path::parent` yields.
        let folder = match fs::location(&folder) {
            fs::Location::ShareRoot if !folder.to_string_lossy().ends_with('\\') => {
                PathBuf::from(format!("{}\\", folder.display()))
            }
            _ => folder,
        };
        let mut place = Some(folder.clone());
        while let Some(p) = place {
            if matches!(fs::location(&p), fs::Location::Server | fs::Location::ShareRoot) && !self.network_places.contains(&p) {
                self.network_places.push(p.clone());
            }
            place = fs::parent(&p);
        }
        self.loader.new_generation();
        self.thumbs.clear();
        self.viewer = None;
        self.selected = None;
        let listing = if fs::is_virtual(&folder) {
            Ok(self.children(&folder).unwrap_or_default().into_iter().map(fs::dir_entry).collect())
        } else {
            fs::list(&folder)
        };
        match listing {
            Ok(mut entries) => {
                fs::sort(&mut entries, self.sort_key, self.sort_desc);
                self.entries = entries;
                self.list_error = None;
            }
            Err(e) => {
                self.entries.clear();
                self.list_error = Some(e.to_string());
            }
        }
        if !self.entries.is_empty() {
            self.selected = Some(0);
        }
        // Stay in the branch the user is browsing; otherwise pick the most specific root.
        if !self.tree_root.as_deref().is_some_and(|r| under_root(&folder, r)) {
            self.tree_root = self
                .roots
                .iter()
                .map(|(_, r)| r)
                .filter(|r| under_root(&folder, r))
                .max_by_key(|r| r.as_os_str().len())
                .cloned();
        }
        if let Some(root) = self.tree_root.clone() {
            let mut ancestor = fs::parent(&folder);
            while let Some(a) = ancestor.filter(|a| under_root(a, &root)) {
                ancestor = fs::parent(&a);
                self.expanded.insert((root.clone(), a));
            }
        }
        self.path_edit = if fs::location(&folder) == fs::Location::Network {
            "Network".to_owned()
        } else {
            folder.display().to_string()
        };
        self.folder = Some(folder);
        self.scroll_tree_to_folder = true;
        self.scroll_to_selected = true;
    }

    /// Re-sort the current listing, keeping the selected item selected and in view.
    fn resort(&mut self) {
        let selected = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone());
        fs::sort(&mut self.entries, self.sort_key, self.sort_desc);
        self.selected = selected.and_then(|p| self.entries.iter().position(|e| e.path == p));
        self.scroll_to_selected = true;
    }

    fn refresh(&mut self) {
        if let Some(f) = self.folder.clone() {
            self.tree_children.remove(&f);
            self.load_folder(f);
        }
    }

    /// Subfolders of `path` for the tree. Local folders are listed inline; network
    /// locations are enumerated in the background and return `None` until ready.
    fn children(&mut self, path: &Path) -> Option<Vec<PathBuf>> {
        let mut children = self.children_raw(path)?;
        // Servers and shares reached by typing a path are often not advertised (e.g. hidden
        // shares, discovery off), so remembered ones are merged in.
        let mut added = false;
        for place in &self.network_places {
            if fs::parent(place).as_deref() == Some(path) && !children.contains(place) {
                children.push(place.clone());
                added = true;
            }
        }
        if added {
            children.sort_by(|a, b| fs::natural_cmp(&fs::display_name(a), &fs::display_name(b)));
        }
        Some(children)
    }

    fn children_raw(&mut self, path: &Path) -> Option<Vec<PathBuf>> {
        if let Some(c) = self.tree_children.get(path) {
            return Some(c.clone());
        }
        if !fs::is_remote(path) {
            let c = fs::subdirs(path);
            self.tree_children.insert(path.to_path_buf(), c.clone());
            return Some(c);
        }
        if self.children_loading.insert(path.to_path_buf()) {
            let (tx, ctx, path) = (self.children_tx.clone(), self.ctx.clone(), path.to_path_buf());
            std::thread::spawn(move || {
                let kids = fs::subdirs_any(&path);
                let _ = tx.send((path, kids));
                ctx.request_repaint();
            });
        }
        None
    }

    /// Windows' own icon for a folder, drive or network location (special folders, custom
    /// folder icons and plain folders alike), so every folder matches Explorer.
    fn shell_icon(&mut self, path: &Path, px: u32) -> Option<egui::TextureId> {
        self.file_icon(path, px)
    }

    /// Windows' icon for any file or program (fetched in the background, cached).
    fn file_icon(&mut self, path: &Path, px: u32) -> Option<egui::TextureId> {
        let key = (path.to_path_buf(), px);
        match self.shell_icons.get(&key) {
            Some(tex) => tex.as_ref().map(|t| t.id()),
            None => {
                self.shell_icons.insert(key, None);
                self.icon_loader.request(path.to_path_buf(), px);
                None
            }
        }
    }

    fn go_up(&mut self) {
        let Some(folder) = self.folder.clone() else { return };
        if let Some(parent) = fs::parent(&folder) {
            self.navigate(parent);
            if let Some(i) = self.entries.iter().position(|e| e.path == folder) {
                self.selected = Some(i);
            }
        }
    }

    fn go_back(&mut self) {
        if let Some(prev) = self.history.pop() {
            self.load_folder(prev);
        }
    }

    fn activate(&mut self, index: usize) {
        let Some(entry) = self.entries.get(index) else { return };
        if entry.is_dir {
            self.navigate(entry.path.clone());
        } else {
            self.open_viewer(index);
        }
    }

    fn open_viewer(&mut self, index: usize) {
        let fresh = self.viewer.is_none();
        self.viewer = Some(Viewer { index, view: ZoomPan::default(), wheel_accum: 0.0 });
        self.selected = Some(index);
        // Only when coming from the browser; moving between images keeps whatever the user chose.
        if fresh && self.settings.open_fullscreen && !self.fullscreen {
            self.set_fullscreen(&self.ctx.clone(), true);
            self.fullscreen_for_viewer = true;
        }
    }

    fn close_viewer(&mut self, ctx: &egui::Context) {
        if let Some(v) = self.viewer.take() {
            self.selected = Some(v.index);
            self.scroll_to_selected = true;
        }
        // Undo fullscreen only if the viewer turned it on; a fullscreen browser stays fullscreen.
        if self.fullscreen && self.fullscreen_for_viewer {
            self.set_fullscreen(ctx, false);
        }
        self.fullscreen_for_viewer = false;
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        self.fullscreen = on;
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

    /// Next/previous image (skipping folders) relative to `from`.
    fn step_image(&self, from: usize, forward: bool) -> Option<usize> {
        let is_img = |i: &usize| !self.entries[*i].is_dir;
        if forward {
            (from + 1..self.entries.len()).find(is_img)
        } else {
            (0..from).rev().find(is_img)
        }
    }

    // ------------------------------------------------------------------ loading

    fn receive(&mut self, ctx: &egui::Context) {
        for (path, has) in self.probe_rx.try_iter() {
            self.has_subdirs.insert(path, has);
        }
        let arrived: Vec<_> = self.children_rx.try_iter().collect();
        for (path, kids) in arrived {
            self.children_loading.remove(&path);
            self.tree_children.insert(path.clone(), kids);
            if self.folder.as_ref() == Some(&path) && fs::is_virtual(&path) {
                let selected = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone());
                self.load_folder(path);
                if let Some(i) = selected.and_then(|p| self.entries.iter().position(|e| e.path == p)) {
                    self.selected = Some(i);
                }
            }
        }
        for (path, px, image) in self.icon_loader.poll() {
            let tex = image.map(|img| ctx.load_texture(format!("icon:{}:{px}", path.display()), img, TextureOptions::LINEAR));
            self.shell_icons.insert((path, px), tex);
        }

        for loaded in self.loader.poll() {
            let name = loaded.path.display().to_string();
            match loaded.kind {
                Kind::Thumb => {
                    // Ignore results for thumbnails we no longer care about (folder changed).
                    if let Some(slot) = self.thumbs.get_mut(&loaded.path) {
                        *slot = match loaded.result {
                            Ok(images) => Thumb::Ready {
                                tex: ctx.load_texture(name, images.main, TextureOptions::LINEAR),
                                last_used: self.frame_no,
                            },
                            Err(_) => Thumb::Failed,
                        };
                    }
                }
                Kind::Full => {
                    if let Some(slot) = self.full.get_mut(&loaded.path) {
                        *slot = match loaded.result {
                            Ok(images) => Full::Ready({
                                let main_tex = ctx.load_texture(name.clone(), images.main, TextureOptions::LINEAR);
                                FullImage {
                                frames: images
                                    .frames
                                    .into_iter()
                                    .enumerate()
                                    .map(|(i, (img, secs))| {
                                        (ctx.load_texture(format!("{name}#{i}"), img, TextureOptions::LINEAR), secs)
                                    })
                                    .collect(),
                                bit_depth: images.bit_depth,
                                format: images.format,
                                sub_layout: images.sub_layout,
                                sub_current: 0,
                                subs: if images.sub_layout.is_some() {
                                    let first = SubTex {
                                        coords: [0, 0, 0],
                                        main: main_tex.clone(),
                                        hdr: None,
                                        size: vec2(images.source_size[0] as f32, images.source_size[1] as f32),
                                    };
                                    std::iter::once(first)
                                        .chain(images.subimages.into_iter().enumerate().map(|(i, sub)| SubTex {
                                            coords: sub.coords,
                                            main: ctx.load_texture(format!("{name}#sub{i}"), sub.main, TextureOptions::LINEAR),
                                            hdr: sub.hdr.zip(self.render_state.as_ref()).and_then(|(hdr, rs)| {
                                                let gpu = crate::hdr_gpu::upload(rs, &hdr.levels)?;
                                                Some(HdrEdit { gpu, auto_exposure: hdr.auto_exposure, tonemap: hdr.tonemap, ev: 0.0 })
                                            }),
                                            size: vec2(sub.size[0] as f32, sub.size[1] as f32),
                                        }))
                                        .collect()
                                } else {
                                    Vec::new()
                                },
                                blur: images.blur.map(|b| ctx.load_texture(format!("{name}#blur"), b, TextureOptions::LINEAR)),
                                ready_at: ctx.input(|i| i.time),
                                paused_at: None,
                                hdr: images.hdr.zip(self.render_state.as_ref()).and_then(|(hdr, rs)| {
                                    let gpu = crate::hdr_gpu::upload(rs, &hdr.levels)?;
                                    Some(HdrEdit { gpu, auto_exposure: hdr.auto_exposure, tonemap: hdr.tonemap, ev: 0.0 })
                                }),
                                main: main_tex,
                                screen: images
                                    .screen
                                    .map(|s| ctx.load_texture(name + "#screen", s, TextureOptions::LINEAR)),
                                source_size: vec2(images.source_size[0] as f32, images.source_size[1] as f32),
                            }}),
                            Err(e) => Full::Failed(e),
                        };
                    }
                }
            }
        }
    }

    fn request_full(&mut self, index: usize) {
        let path = self.entries[index].path.clone();
        if let Some(pos) = self.full_lru.iter().position(|p| *p == path) {
            self.full_lru.remove(pos);
        }
        self.full_lru.push_back(path.clone());
        if !self.full.contains_key(&path) {
            self.full.insert(path.clone(), Full::Pending);
            self.loader.request_full(path, self.max_texture_side, self.screen_side);
        }
    }

    /// Make sure the current image and its neighbours are loaded or loading; evict old ones.
    fn update_full_cache(&mut self) {
        // The viewer's image, or the preview pane's selection.
        let index = match &self.viewer {
            Some(v) => v.index,
            None if self.settings.three_pane => match self.selected {
                Some(i) if self.entries.get(i).is_some_and(|e| !e.is_dir) => i,
                _ => return,
            },
            None => return,
        };
        let mut wanted = vec![index];
        let (mut fwd, mut back) = (index, index);
        for _ in 0..PREFETCH {
            if let Some(n) = self.step_image(fwd, true) {
                wanted.push(n);
                fwd = n;
            }
            if let Some(p) = self.step_image(back, false) {
                wanted.push(p);
                back = p;
            }
        }
        // Request in reverse priority so the current image ends up most-recently-used,
        // but send the current image first so it is decoded first.
        for &i in &wanted {
            self.request_full(i);
        }
        self.request_full(index);

        let keep: HashSet<PathBuf> = wanted.iter().map(|&i| self.entries[i].path.clone()).collect();
        while self.full_lru.len() > FULL_CACHE {
            let Some(pos) = self.full_lru.iter().position(|p| !keep.contains(p)) else { break };
            if let Some(p) = self.full_lru.remove(pos) {
                self.full.remove(&p);
            }
        }
    }

    fn evict_thumbs(&mut self) {
        // Folder icons accumulate while browsing; past a few thousand, keep only the sidebar's
        // special folders (the rest reload on demand from the background icon thread).
        if self.shell_icons.len() > 4000 {
            let special = &self.special;
            self.shell_icons.retain(|(path, _), _| special.contains(path));
        }
        let live = self.thumbs.values().filter(|t| matches!(t, Thumb::Ready { .. })).count();
        if live <= MAX_LIVE_THUMBS {
            return;
        }
        let mut ages: Vec<(u64, PathBuf)> = self
            .thumbs
            .iter()
            .filter_map(|(p, t)| match t {
                Thumb::Ready { last_used, .. } => Some((*last_used, p.clone())),
                _ => None,
            })
            .collect();
        ages.sort_unstable_by_key(|(age, _)| *age);
        for (_, p) in ages.into_iter().take(live - MAX_LIVE_THUMBS * 3 / 4) {
            self.thumbs.remove(&p);
        }
    }

    // ------------------------------------------------------------------ browser UI

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if icons::button(ui, Icon::Back, !self.history.is_empty(), "Back (Alt+←)").clicked() {
                self.go_back();
            }
            let can_go_up = self.folder.as_deref().and_then(fs::parent).is_some();
            if icons::button(ui, Icon::Up, can_go_up, "Up (Backspace)").clicked() {
                self.go_up();
            }
            if icons::button(ui, Icon::Refresh, true, "Refresh (F5)").clicked() {
                self.refresh();
            }
            ui.add_space(6.0);
            // Right-hand controls are laid out from the right edge (so the cog is always flush
            // right); the address bar then takes whatever width is left. Items are added in
            // reverse visual order.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                if icons::button(ui, Icon::Cog, true, "Settings").clicked() {
                    self.settings_page = Some(self.settings_page.unwrap_or(SettingsPage::Appearance));
                }
                ui.add_space(4.0);
                if icons::toggle(ui, Icon::ThreePane, self.settings.three_pane, "Three panes: folders, thumbnails and preview")
                    .clicked()
                {
                    self.settings.three_pane = true;
                }
                if icons::toggle(ui, Icon::TwoPane, !self.settings.three_pane, "Two panes: folders and thumbnails").clicked() {
                    self.settings.three_pane = false;
                }
                ui.add_space(8.0);
                ui.add(egui::Slider::new(&mut self.thumb_size, 80.0..=320.0).show_value(false));
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Grid, theme::p().text_dim);
                ui.add_space(8.0);

                let before = (self.sort_key, self.sort_desc);
                let (dir_icon, dir_tip) = if self.sort_desc { (Icon::Down, "Descending") } else { (Icon::Up, "Ascending") };
                if icons::button(ui, dir_icon, true, dir_tip).clicked() {
                    self.sort_desc = !self.sort_desc;
                }
                egui::ComboBox::from_id_salt("sort")
                    .selected_text(self.sort_key.label())
                    .width(130.0)
                    .show_ui(ui, |ui| {
                        for key in SortKey::ALL {
                            ui.selectable_value(&mut self.sort_key, key, key.label());
                        }
                    });
                let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
                icons::paint(ui.painter(), r, Icon::Sort, theme::p().text_dim);
                if before != (self.sort_key, self.sort_desc) {
                    self.resort();
                }
                ui.add_space(8.0);

                let edit = egui::TextEdit::singleline(&mut self.path_edit)
                    .desired_width(ui.available_width())
                    .margin(vec2(8.0, 5.0));
                let resp = ui.add(edit);
                if resp.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                    let typed = self.path_edit.trim();
                    let p = if typed.eq_ignore_ascii_case("network") { PathBuf::from(shell::NETWORK) } else { PathBuf::from(typed) };
                    if p.is_dir() || fs::is_virtual(&p) {
                        self.navigate(p);
                    } else if p.is_file() {
                        if let Some(parent) = p.parent() {
                            self.navigate(parent.to_path_buf());
                        }
                        if let Some(i) = self.entries.iter().position(|e| e.path == p) {
                            self.open_viewer(i);
                        }
                    }
                }
            });
        });
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(mut page) = self.settings_page else { return };
        let p = theme::p();
        let frame = egui::Frame::NONE
            .fill(p.panel)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(14))
            .shadow(egui::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(110) });
        let mut close = false;
        let modal = egui::Modal::new(egui::Id::NULL.with("cyclops-settings")).frame(frame).show(ctx, |ui| {
            let height = 520.0_f32.min(ctx.content_rect().height() - 80.0);
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing = vec2(0.0, 0.0);
                // Sidebar
                egui::Frame::NONE
                    .fill(p.bg)
                    .corner_radius(CornerRadius { nw: 14, sw: 14, ne: 0, se: 0 })
                    .inner_margin(egui::Margin::symmetric(12, 18))
                    .show(ui, |ui| {
                        ui.vertical(|ui| {
                            ui.set_width(176.0);
                            ui.set_height(height - 36.0);
                            ui.spacing_mut().item_spacing.y = 4.0;
                            ui.label(egui::RichText::new("Settings").size(18.0).color(p.text_strong));
                            ui.add_space(14.0);
                            for candidate in SettingsPage::ALL {
                                let selected = page == candidate;
                                let text = egui::RichText::new(candidate.label())
                                    .size(14.0)
                                    .color(if selected { p.text_strong } else { p.text });
                                let button = egui::Button::selectable(selected, text).min_size(vec2(176.0, 32.0));
                                if ui.add(button).clicked() {
                                    page = candidate;
                                }
                            }
                            ui.add_space((ui.available_height() - 18.0).max(0.0));
                            ui.label(egui::RichText::new(concat!("Cyclops ", env!("CARGO_PKG_VERSION"))).small().color(p.text_dim));
                        });
                    });
                // Content
                egui::Frame::NONE.inner_margin(egui::Margin { left: 28, right: 20, top: 16, bottom: 20 }).show(ui, |ui| {
                    ui.vertical(|ui| {
                        // Fixed size for every section, so the window never resizes (and re-centres)
                        // when switching between them.
                        ui.set_width(560.0);
                        ui.set_max_width(560.0);
                        ui.set_height(height - 36.0);
                        ui.spacing_mut().item_spacing = vec2(8.0, 6.0);
                        ui.horizontal(|ui| {
                            ui.label(egui::RichText::new(page.label()).size(20.0).color(p.text_strong));
                            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                if icons::button(ui, Icon::Close, true, "Close (Esc)").clicked() {
                                    close = true;
                                }
                            });
                        });
                        ui.add_space(14.0);
                        egui::ScrollArea::vertical().max_height(height - 90.0).auto_shrink([false, true]).show(ui, |ui| {
                            ui.vertical(|ui| match page {
                                SettingsPage::Appearance => self.settings_appearance(ui),
                                SettingsPage::Browsing => self.settings_browsing(ui),
                                SettingsPage::Viewer => self.settings_viewer(ui),
                                SettingsPage::Editor => self.settings_editor(ui),
                                SettingsPage::Developer => self.settings_developer(ui),
                            });
                        });
                    });
                });
            });
        });
        self.settings_page = if close || modal.should_close() { None } else { Some(page) };
    }

    fn settings_appearance(&mut self, ui: &mut egui::Ui) {
        setting_row(ui, "Skin", "System follows the Windows light/dark setting.", |ui| {
            let options = Skin::ALL.map(|s| (s, s.label()));
            if segmented(ui, &mut self.settings.skin, &options) {
                ui.ctx().set_theme(self.settings.skin.preference());
            }
        });
        setting_row(
            ui,
            "Window material",
            "Mica tints the window with your wallpaper; Acrylic shows a frosted view of what's behind it (Windows 11).",
            |ui| {
                let options = crate::chrome::Material::ALL.map(|m| (m, m.label()));
                segmented(ui, &mut self.settings.material, &options);
            },
        );
    }

    fn settings_browsing(&mut self, ui: &mut egui::Ui) {
        setting_row(ui, "Layout", "Show a large preview of the selection next to the thumbnails.", |ui| {
            segmented(ui, &mut self.settings.three_pane, &[(false, "Two panes"), (true, "With preview")]);
        });
        setting_row(ui, "Thumbnail size", "", |ui| {
            // Right-to-left row: value label first so it ends up after the slider.
            ui.label(egui::RichText::new(format!("{:.0} px", self.thumb_size)).color(theme::p().text_dim));
            ui.add(egui::Slider::new(&mut self.thumb_size, 80.0..=320.0).show_value(false));
        });
        setting_row(ui, "Show file date", "Modification time under each thumbnail.", |ui| {
            toggle(ui, &mut self.settings.show_dates);
        });
    }

    fn settings_viewer(&mut self, ui: &mut egui::Ui) {
        setting_row(ui, "Mouse wheel", "Ctrl + wheel always zooms.", |ui| {
            segmented(ui, &mut self.settings.wheel_zooms, &[(true, "Zoom"), (false, "Next image")]);
        });
        setting_row(ui, "Info bar", "File name, size and zoom at the bottom of the viewer.", |ui| {
            toggle(ui, &mut self.settings.show_info_bar);
        });
        setting_row(ui, "Open images fullscreen", "Double-click or Enter shows the image fullscreen; Esc returns.", |ui| {
            toggle(ui, &mut self.settings.open_fullscreen);
        });
        setting_row(ui, "Enlarge small images", "Fit scales images up to fill the screen, not just down.", |ui| {
            toggle(ui, &mut self.settings.enlarge_small);
        });
    }

    fn settings_editor(&mut self, ui: &mut egui::Ui) {
        let p = theme::p();
        ui.label(egui::RichText::new("Press E, or right-click an image and choose Edit, to open it in this program.").color(p.text_dim));
        ui.add_space(12.0);
        let custom = self.settings.editor.clone().filter(|c| !self.detected_editors.contains(c));
        let mut choices: Vec<Option<PathBuf>> = vec![None];
        choices.extend(self.detected_editors.iter().cloned().map(Some));
        choices.extend(custom.map(Some));
        let icon_px = (32.0 * ui.ctx().pixels_per_point()).ceil() as u32;
        for choice in choices {
            let icon = choice.as_ref().and_then(|exe| self.file_icon(exe, icon_px));
            let selected = self.settings.editor == choice;
            let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 58.0), Sense::click());
            let painter = ui.painter();
            let (fill, stroke) = if selected {
                (p.accent_soft, Stroke::new(1.5, p.accent))
            } else if resp.hovered() {
                (p.surface_hover, Stroke::new(1.0, p.border))
            } else {
                (p.surface, Stroke::new(1.0, p.border))
            };
            painter.rect(rect, CornerRadius::same(10), fill, stroke, egui::StrokeKind::Inside);
            let icon_rect = Rect::from_center_size(pos2(rect.min.x + 32.0, rect.center().y), vec2(32.0, 32.0));
            match icon {
                Some(tex) => {
                    painter.image(tex, icon_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => icons::paint(painter, icon_rect.shrink(4.0), Icon::TwoPane, p.text_dim),
            }
            let (title, detail) = match &choice {
                Some(exe) => (crate::editor::name(exe), exe.display().to_string()),
                None => ("Windows default".to_owned(), "The program Windows uses to edit this file type".to_owned()),
            };
            let text_x = rect.min.x + 62.0;
            painter.text(pos2(text_x, rect.center().y - 9.0), Align2::LEFT_CENTER, title, FontId::proportional(14.5), p.text_strong);
            let mut job = LayoutJob::single_section(
                detail,
                TextFormat { font_id: FontId::proportional(11.5), color: p.text_dim, ..Default::default() },
            );
            job.wrap = TextWrapping::truncate_at_width(rect.max.x - 48.0 - text_x);
            let galley = painter.layout_job(job);
            painter.galley(pos2(text_x, rect.center().y + 3.0), galley, p.text_dim);
            // Radio marker
            let radio = pos2(rect.max.x - 24.0, rect.center().y);
            painter.circle_stroke(radio, 8.0, Stroke::new(1.5, if selected { p.accent } else { p.text_dim }));
            if selected {
                painter.circle_filled(radio, 4.5, p.accent);
            }
            if resp.clicked() {
                self.settings.editor = choice;
            }
            ui.add_space(8.0);
        }
        ui.add_space(4.0);
        let browse = egui::Button::new("Choose another program…")
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.border))
            .corner_radius(CornerRadius::same(8))
            .min_size(vec2(0.0, 32.0));
        if ui.add(browse).clicked() {
            if let Some(exe) = crate::editor::pick_program() {
                self.settings.editor = Some(exe);
            }
        }
    }

    fn settings_developer(&mut self, ui: &mut egui::Ui) {
        setting_row(ui, "Developer log", "Decode times, GPU and window setup, and warnings. Startup lines are included too.", |ui| {
            toggle(ui, &mut self.settings.perf_log);
        });
        if self.settings.perf_log {
            // Button first so the (truncated) path can take the remaining width.
            ui.horizontal(|ui| {
                if ui.small_button("Show in Explorer").clicked() {
                    ops::reveal(&crate::perflog::path());
                }
                ui.add(egui::Label::new(egui::RichText::new(crate::perflog::path().display().to_string()).small().color(theme::p().text_dim)).truncate());
            });
        }
    }

    fn folder_tree(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            // Shortcuts, drives and network form three groups separated by a divider.
            let group = |p: &Path| match fs::location(p) {
                fs::Location::Network => 2,
                _ if p.parent().is_none() => 1,
                _ => 0,
            };
            let roots = self.roots.clone();
            let mut last_group = None;
            for (label, path) in &roots {
                let g = group(path);
                if last_group.is_some_and(|last| last != g) {
                    ui.add_space(3.0);
                    ui.separator();
                    ui.add_space(3.0);
                }
                last_group = Some(g);
                self.tree_node(ui, path, path, label, 0);
            }
        });
        self.scroll_tree_to_folder = false;
    }

    fn tree_node(&mut self, ui: &mut egui::Ui, root: &Path, path: &Path, label: &str, depth: usize) {
        let key = (root.to_path_buf(), path.to_path_buf());
        let expanded = self.expanded.contains(&key);
        let is_current = self.folder.as_deref() == Some(path) && self.tree_root.as_deref() == Some(root);

        let row_h = 24.0;
        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width().max(120.0), row_h), Sense::click());
        let indent = 4.0 + depth as f32 * 14.0;
        let chevron = Rect::from_min_size(pos2(rect.min.x + indent, rect.min.y + 4.0), vec2(16.0, 16.0));
        let on_chevron = resp.interact_pointer_pos().is_some_and(|p| chevron.expand(3.0).contains(p));
        let shell_tex = self.shell_icon(path, (16.0 * ui.ctx().pixels_per_point()).ceil() as u32);
        // Like Explorer: no chevron on folders without subfolders (unknown yet = none drawn).
        let expandable = ui.is_rect_visible(rect).then(|| self.expandable(path)).flatten().unwrap_or(false)
            || self.tree_children.get(path).is_some_and(|c| !c.is_empty());

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if is_current {
                painter.rect_filled(rect, CornerRadius::same(6), theme::p().accent_soft);
            } else if resp.hovered() {
                painter.rect_filled(rect, CornerRadius::same(6), theme::p().surface_hover);
            }
            if expandable {
                let chevron_icon = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
                icons::paint(painter, chevron.shrink(2.0), chevron_icon, theme::p().text_dim);
            }
            let icon_rect = Rect::from_min_size(pos2(chevron.max.x + 4.0, rect.min.y + 4.0), vec2(16.0, 16.0));
            if let Some(tex) = shell_tex {
                painter.image(tex, icon_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            } else {
                let (icon, icon_color) = if depth == 0 { (Icon::Drive, theme::p().text_dim) } else { (Icon::Folder, theme::p().accent) };
                icons::paint(painter, icon_rect, icon, icon_color);
            }
            let color = if is_current { theme::p().text_strong } else { theme::p().text };
            let galley = painter.layout_no_wrap(label.to_owned(), FontId::proportional(13.5), color);
            let text_pos = pos2(icon_rect.max.x + 7.0, rect.center().y - galley.size().y / 2.0);
            painter.with_clip_rect(rect).galley(text_pos, galley, color);
        }

        if expandable && (resp.clicked() && on_chevron || resp.double_clicked()) {
            if expanded {
                self.expanded.remove(&key);
            } else {
                self.expanded.insert(key.clone());
            }
        } else if resp.clicked() {
            self.tree_root = Some(root.to_path_buf());
            self.navigate(path.to_path_buf());
        }
        if !fs::is_virtual(path) {
            let mut action = None;
            resp.context_menu(|ui| {
                if ui.button("New folder").clicked() {
                    action = Some(FileAction::NewFolder(path.to_path_buf()));
                    ui.close();
                }
                if ui.button("Show in Explorer").clicked() {
                    ops::reveal(path);
                    ui.close();
                }
            });
            if action.is_some() {
                // Create it in the folder that was right-clicked, keeping that branch of the tree.
                self.tree_root = Some(root.to_path_buf());
                self.expanded.insert((root.to_path_buf(), path.to_path_buf()));
                self.pending_action = action;
            }
        }
        if is_current && self.scroll_tree_to_folder {
            resp.scroll_to_me(Some(egui::Align::Center));
        }
        if self.expanded.contains(&key) {
            match self.children(path) {
                Some(children) => {
                    for child in &children {
                        self.tree_node(ui, root, child, &fs::display_name(child), depth + 1);
                    }
                }
                None => {
                    ui.horizontal(|ui| {
                        ui.add_space(4.0 + (depth + 1) as f32 * 14.0 + 20.0);
                        ui.add(egui::Spinner::new().size(12.0).color(theme::p().text_dim));
                        ui.label(egui::RichText::new("Searching…").color(theme::p().text_dim).small());
                    });
                }
            }
        }
    }

    /// Whether a tree folder can expand: `None` while still being probed.
    fn expandable(&mut self, path: &Path) -> Option<bool> {
        if let Some(children) = self.tree_children.get(path) {
            return Some(!children.is_empty());
        }
        // Never probe the network just to draw a chevron.
        if fs::is_remote(path) {
            return Some(true);
        }
        if let Some(&has) = self.has_subdirs.get(path) {
            return Some(has);
        }
        self.has_subdirs.insert(path.to_path_buf(), false);
        let _ = self.probe_tx.send(path.to_path_buf());
        // Placeholder until the probe answers (cached as false above, overwritten by the result).
        None
    }

    fn grid_keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        self.type_to_find(ctx);
        let n = self.entries.len();
        let cols = self.grid_cols.max(1);
        let page = cols * ((self.grid_viewport.height() / self.cell_size().y).floor() as usize).max(1);
        let mut moved = None;
        let cur = self.selected.unwrap_or(0);
        ctx.input(|i| {
            if n > 0 {
                if i.key_pressed(Key::ArrowRight) {
                    moved = Some((cur + 1).min(n - 1));
                }
                if i.key_pressed(Key::ArrowLeft) {
                    moved = Some(cur.saturating_sub(1));
                }
                if i.key_pressed(Key::ArrowDown) {
                    moved = Some((cur + cols).min(n - 1));
                }
                if i.key_pressed(Key::ArrowUp) {
                    moved = Some(cur.saturating_sub(cols));
                }
                if i.key_pressed(Key::PageDown) {
                    moved = Some((cur + page).min(n - 1));
                }
                if i.key_pressed(Key::PageUp) {
                    moved = Some(cur.saturating_sub(page));
                }
                if i.key_pressed(Key::Home) {
                    moved = Some(0);
                }
                if i.key_pressed(Key::End) {
                    moved = Some(n - 1);
                }
            }
        });
        if let Some(m) = moved {
            self.selected = Some(m);
            self.scroll_to_selected = true;
        }
        if ctx.input(|i| i.modifiers.ctrl && i.modifiers.shift && i.key_pressed(Key::N)) {
            if let Some(folder) = self.folder.clone().filter(|f| !fs::is_virtual(f)) {
                self.pending_action = Some(FileAction::NewFolder(folder));
            }
        }
        if let Some(sel) = self.selected {
            let (delete, rename) = ctx.input(|i| (i.key_pressed(Key::Delete), i.key_pressed(Key::F2)));
            let edit = ctx.input(|i| i.modifiers.command && i.key_pressed(Key::E));
            if delete {
                self.pending_action = Some(FileAction::Delete(sel));
            } else if rename {
                self.pending_action = Some(FileAction::Rename(sel));
            } else if edit {
                self.pending_action = Some(FileAction::Edit(sel));
            }
        }
        let (enter, back, alt_left, refresh) = ctx.input(|i| {
            (
                !i.modifiers.alt && i.key_pressed(Key::Enter),
                i.key_pressed(Key::Backspace),
                i.modifiers.alt && i.key_pressed(Key::ArrowLeft),
                i.key_pressed(Key::F5),
            )
        });
        if enter {
            if let Some(s) = self.selected {
                self.activate(s);
            }
        } else if alt_left {
            self.go_back();
        } else if back {
            self.go_up();
        } else if refresh {
            self.refresh();
        }
    }

    fn cell_size(&self) -> Vec2 {
        // Square tiles; the name and date sit inside the bottom of the square.
        Vec2::splat(self.thumb_size + 12.0)
    }

    fn grid(&mut self, ui: &mut egui::Ui) {
        // Right-clicking empty space acts on the current folder. Registered before the tiles,
        // so tiles (added later, on top) keep their own menus.
        if let Some(folder) = self.folder.clone().filter(|f| !fs::is_virtual(f) && self.list_error.is_none()) {
            let bg = ui.interact(ui.max_rect(), ui.scope_id().with("grid-background"), Sense::click());
            let mut action = None;
            bg.context_menu(|ui| folder_menu(ui, &folder, &mut action));
            if action.is_some() {
                self.pending_action = action;
            }
        }
        if let Some(err) = &self.list_error {
            ui.centered_and_justified(|ui| ui.label(egui::RichText::new(err.clone()).color(theme::p().text_dim)));
            return;
        }
        if self.entries.is_empty() {
            let searching = self.folder.as_ref().is_some_and(|f| self.children_loading.contains(f));
            let text = if searching { "Searching the network…" } else { "No images here" };
            ui.centered_and_justified(|ui| ui.label(egui::RichText::new(text).color(theme::p().text_dim)));
            return;
        }

        let gap = 10.0;
        let cell = self.cell_size();
        let avail_w = ui.available_width();
        let cols = (((avail_w - gap) / (cell.x + gap)).floor() as usize).max(1);
        self.grid_cols = cols;
        // Spread leftover space evenly between columns so the grid is always justified.
        let gap_x = ((avail_w - cols as f32 * cell.x) / (cols as f32 + 1.0)).max(gap);
        let row_h = cell.y + gap;
        let rows = self.entries.len().div_ceil(cols);
        let total_h = rows as f32 * row_h + gap;

        let mut area = egui::ScrollArea::vertical()
            .id_salt(("grid", self.folder.clone()))
            .auto_shrink(false)
            .wheel_scroll_multiplier(vec2(1.0, 1.6));
        if self.scroll_to_selected {
            self.scroll_to_selected = false;
            if let Some(sel) = self.selected {
                let top = (sel / cols) as f32 * row_h;
                let bottom = top + row_h + gap;
                let vp = self.grid_viewport;
                if !vp.is_positive() {
                    area = area.vertical_scroll_offset((top - gap).max(0.0));
                } else if top < vp.min.y {
                    area = area.vertical_scroll_offset(top);
                } else if bottom > vp.max.y {
                    area = area.vertical_scroll_offset(bottom - vp.height());
                }
            }
        }

        let mut clicked = None;
        let mut activated = None;
        let mut menu_action = None;
        let edit_label = self.edit_label();
        area.show_viewport(ui, |ui, viewport| {
            self.grid_viewport = viewport;
            ui.set_min_size(vec2(avail_w, total_h));
            let origin = ui.max_rect().min;
            // One extra row above/below keeps thumbnails ready just before they scroll into view.
            let first_row = ((viewport.min.y - gap) / row_h).floor().max(0.0) as usize;
            let last_row = (((viewport.max.y) / row_h).ceil() as usize + 1).min(rows);
            let first = (first_row.saturating_sub(1)) * cols;
            let last = (last_row * cols).min(self.entries.len());
            for idx in first..last {
                let (row, col) = (idx / cols, idx % cols);
                let min = origin + vec2(gap_x + col as f32 * (cell.x + gap_x), gap + row as f32 * row_h);
                let rect = Rect::from_min_size(min, cell);
                let resp = ui.interact(rect, ui.scope_id().with(("tile", idx)), Sense::click());
                if resp.clicked() || resp.secondary_clicked() {
                    clicked = Some(idx);
                }
                resp.context_menu(|ui| file_menu(ui, &edit_label, idx, &self.entries[idx], true, &mut menu_action));
                if resp.double_clicked() {
                    activated = Some(idx);
                }
                if ui.is_rect_visible(rect) {
                    self.paint_tile(ui, idx, rect, resp.hovered());
                }
            }
        });
        if let Some(i) = clicked {
            self.selected = Some(i);
        }
        if let Some(i) = activated {
            self.activate(i);
        }
        if menu_action.is_some() {
            self.pending_action = menu_action;
        }
    }

    fn paint_tile(&mut self, ui: &egui::Ui, idx: usize, rect: Rect, hovered: bool) {
        let dir_icon = if self.entries[idx].is_dir {
            let path = self.entries[idx].path.clone();
            self.shell_icon(&path, 128)
        } else {
            None
        };
        let entry = &self.entries[idx];
        let selected = self.selected == Some(idx);
        let painter = ui.painter();
        let radius = CornerRadius::same(8);

        let (fill, stroke) = if selected {
            (theme::p().accent_soft, Stroke::new(1.5, theme::p().accent))
        } else if hovered {
            (theme::p().surface_hover, Stroke::new(1.0, theme::p().border))
        } else {
            (theme::p().surface, Stroke::NONE)
        };
        painter.rect(rect, radius, fill, stroke, egui::StrokeKind::Inside);

        let img_area = Rect::from_min_max(rect.min + vec2(6.0, 6.0), pos2(rect.max.x - 6.0, rect.max.y - 46.0));
        if entry.is_dir {
            let side = self.thumb_size * 0.45;
            let icon_rect = Rect::from_center_size(img_area.center(), vec2(side, side));
            match dir_icon {
                Some(tex) => {
                    painter.image(tex, icon_rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => icons::paint(painter, icon_rect, Icon::Folder, theme::p().accent),
            }
        } else {
            let state = self.thumbs.entry(entry.path.clone()).or_insert_with(|| {
                self.loader.request_thumb(entry.path.clone(), THUMB_DECODE_SIDE);
                Thumb::Pending
            });
            match state {
                Thumb::Ready { tex, last_used } => {
                    *last_used = self.frame_no;
                    let size = tex.size_vec2();
                    let scale = (img_area.width() / size.x).min(img_area.height() / size.y).min(1.0);
                    let r = Rect::from_center_size(img_area.center(), size * scale);
                    painter.image(tex.id(), r, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                    if entry.is_video {
                        video_badge(painter, r.max - vec2(14.0, 14.0), 10.0);
                    }
                }
                Thumb::Pending => {
                    painter.rect_filled(img_area.shrink(img_area.width() * 0.12), CornerRadius::same(6), theme::p().bg.gamma_multiply(0.6));
                }
                Thumb::Failed => {
                    icons::paint(painter, Rect::from_center_size(img_area.center(), vec2(28.0, 28.0)), Icon::Warning, theme::p().text_dim);
                }
            }
        }

        let label_w = rect.width() - 14.0;
        let mut job = LayoutJob::single_section(
            entry.name.clone(),
            TextFormat {
                font_id: FontId::proportional(12.0),
                color: if selected { theme::p().text_strong } else { theme::p().text_dim },
                ..Default::default()
            },
        );
        job.wrap = TextWrapping::truncate_at_width(label_w);
        let galley = painter.layout_job(job);
        let pos = pos2(rect.center().x - galley.size().x / 2.0, rect.max.y - 38.0);
        painter.galley(pos, galley, theme::p().text_dim);
        if self.settings.show_dates && !entry.modified_label.is_empty() {
            let color = if selected { theme::p().text } else { theme::p().text_dim.gamma_multiply(0.75) };
            painter.text(
                pos2(rect.center().x, rect.max.y - 10.0),
                Align2::CENTER_BOTTOM,
                &entry.modified_label,
                FontId::proportional(11.0),
                color,
            );
        }
    }

    /// Explorer-style type-to-find: characters typed within a second form a prefix and select the
    /// next file whose name starts with it; repeating one letter cycles through its matches.
    fn type_to_find(&mut self, ctx: &egui::Context) {
        const RESET_AFTER: f64 = 1.0;
        let (typed, now) = ctx.input(|i| {
            let typed: String = if i.modifiers.command || i.modifiers.alt {
                String::new()
            } else {
                i.events.iter().filter_map(|e| if let egui::Event::Text(t) = e { Some(t.as_str()) } else { None }).collect()
            };
            (typed, i.time)
        });
        if typed.is_empty() || self.entries.is_empty() {
            return;
        }
        let (buffer, last) = &mut self.typeahead;
        if now - *last > RESET_AFTER {
            buffer.clear();
        }
        *last = now;
        // A leading space would be invisible and is almost never intended.
        if buffer.is_empty() && typed.trim().is_empty() {
            return;
        }
        buffer.push_str(&typed.to_lowercase());
        ctx.request_repaint_after(std::time::Duration::from_secs_f64(RESET_AFTER));

        let n = self.entries.len();
        let current = self.selected.unwrap_or(0).min(n - 1);
        let first = buffer.chars().next().unwrap_or_default();
        let repeating = buffer.chars().all(|c| c == first);
        // Repeating one letter cycles to the next match; a longer prefix refines from the current file.
        let (prefix, start) = if repeating && buffer.chars().count() > 1 {
            (first.to_string(), current + 1)
        } else if buffer.chars().count() == 1 {
            (buffer.clone(), current + 1)
        } else {
            (buffer.clone(), current)
        };
        let found = (0..n)
            .map(|k| (start + k) % n)
            .find(|&i| self.entries[i].name.to_lowercase().starts_with(&prefix));
        if let Some(i) = found {
            self.selected = Some(i);
            self.scroll_to_selected = true;
        }
    }

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let (prefix, last) = &self.typeahead;
            if !prefix.is_empty() && ui.input(|i| i.time) - last <= 1.0 {
                ui.label(egui::RichText::new(format!("Find: {prefix}")).small().color(theme::p().accent));
                ui.separator();
            }
            let dirs = self.entries.iter().filter(|e| e.is_dir).count();
            let videos = self.entries.iter().filter(|e| e.is_video).count();
            let images = self.entries.len() - dirs - videos;
            let mut counts = format!("{images} images");
            if videos > 0 {
                counts += &format!(" · {videos} videos");
            }
            counts += &format!(" · {dirs} folders");
            ui.label(egui::RichText::new(counts).color(theme::p().text_dim).small());
            if let Some(e) = self.selected.and_then(|i| self.entries.get(i)) {
                ui.separator();
                let mut text = e.name.clone();
                if !e.is_dir {
                    text += &format!("  ·  {}", fs::human_size(e.size));
                }
                if !e.modified_label.is_empty() {
                    text += &format!("  ·  {}", e.modified_label);
                }
                ui.label(egui::RichText::new(text).small());
            }
        });
    }

    // ------------------------------------------------------------------ viewer UI

    fn viewer_input(&mut self, ctx: &egui::Context) {
        let Some(v) = &self.viewer else { return };
        let index = v.index;
        let (delete, rename) = ctx.input(|i| (i.key_pressed(Key::Delete), i.key_pressed(Key::F2)));
        let edit = ctx.input(|i| (i.modifiers.is_none() || i.modifiers.command) && i.key_pressed(Key::E));
        if delete {
            self.pending_action = Some(FileAction::Delete(index));
        } else if rename {
            self.pending_action = Some(FileAction::Rename(index));
        } else if edit {
            self.pending_action = Some(FileAction::Edit(index));
        }
        // Ctrl+arrows step through a texture's mips (↑ smaller / ↓ larger) and layers/faces
        // (← →): keyboard-layout independent, and plain arrows keep changing images.
        let (mip_down, mip_up, slice_prev, slice_next) = ctx.input(|i| {
            let ctrl = i.modifiers.ctrl;
            (
                ctrl && i.key_pressed(Key::ArrowDown),
                ctrl && i.key_pressed(Key::ArrowUp),
                ctrl && i.key_pressed(Key::ArrowLeft),
                ctrl && i.key_pressed(Key::ArrowRight),
            )
        });
        if let Some(path) = self.entries.get(index).map(|e| e.path.clone()) {
            match (mip_down, mip_up, slice_prev, slice_next) {
                (true, ..) => self.step_sub(&path, 0, -1),
                (_, true, ..) => self.step_sub(&path, 0, 1),
                (_, _, true, _) => self.step_sub(&path, 1, -1),
                (.., true) => self.step_sub(&path, 1, 1),
                _ => {}
            }
        }
        let is_video = self.entries.get(index).is_some_and(|e| e.is_video);
        let animated = self.entries.get(index).and_then(|e| match self.full.get(&e.path) {
            Some(Full::Ready(img)) if !img.frames.is_empty() => Some(e.path.clone()),
            _ => None,
        });
        if animated.is_some() && ctx.input(|i| i.key_pressed(Key::Space)) {
            let now = ctx.input(|i| i.time);
            if let Some(Full::Ready(img)) = animated.as_ref().and_then(|p| self.full.get_mut(p)) {
                img.toggle_animation(now);
            }
        }
        let plays = is_video || animated.is_some();
        let (play_pause, mute) = ctx.input(|i| (is_video && i.key_pressed(Key::Space), is_video && i.key_pressed(Key::M)));
        if let Some((_, Ok(player))) = &mut self.video {
            if play_pause {
                player.toggle();
            }
            if mute {
                self.settings.video_muted = !player.muted();
                player.set_muted(self.settings.video_muted);
            }
        }
        let (next, prev, first, last, esc, fit, one, zoom_in, zoom_out) = ctx.input(|i| {
            (
                (!i.modifiers.ctrl && (i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown)))
                    || (!plays && i.key_pressed(Key::Space))
                    || i.key_pressed(Key::PageDown),
                (!i.modifiers.ctrl && (i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp)))
                    || i.key_pressed(Key::Backspace)
                    || i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Escape) || (!i.modifiers.alt && i.key_pressed(Key::Enter)),
                i.key_pressed(Key::Num0) || i.key_pressed(Key::Slash),
                i.key_pressed(Key::Num1),
                i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals),
                i.key_pressed(Key::Minus),
            )
        });
        let target = if next {
            self.step_image(index, true)
        } else if prev {
            self.step_image(index, false)
        } else if first {
            (0..self.entries.len()).find(|&i| !self.entries[i].is_dir)
        } else if last {
            (0..self.entries.len()).rev().find(|&i| !self.entries[i].is_dir)
        } else {
            None
        };
        if let Some(t) = target {
            self.open_viewer(t);
        }
        if esc {
            // Fullscreen the user chose inside the viewer: Esc leaves fullscreen first.
            if self.fullscreen && !self.fullscreen_for_viewer && ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.set_fullscreen(ctx, false);
            } else {
                self.close_viewer(ctx);
            }
            return;
        }
        if let Some(v) = &mut self.viewer {
            if fit {
                v.view = ZoomPan::default();
            }
            if one {
                v.view.zoom = Some(1.0 / ctx.pixels_per_point());
                v.view.anim = None;
            }
        }
        self.pending_key_zoom = if zoom_in { 1.25 } else if zoom_out { 0.8 } else { 1.0 };
    }

    fn viewer(&mut self, ui: &mut egui::Ui) {
        let ctx = ui.ctx().clone();
        let rect = ui.max_rect();
        let resp = ui.allocate_rect(rect, Sense::click_and_drag());
        let ppp = ctx.pixels_per_point();
        let Some(index) = self.viewer.as_ref().map(|v| v.index) else { return };
        let entry = self.entries[index].clone();

        // Videos: (re)open the player for this file and pull the next frame.
        let mut video_frame = None;
        let mut video_size = None;
        if entry.is_video {
            if self.video.as_ref().is_none_or(|(p, _)| *p != entry.path) {
                self.video = None; // stop the previous one first
                let player = VideoPlayer::open(&entry.path, &ctx, self.settings.video_muted, self.settings.video_volume).map_err(|e| e.to_string());
                self.video = Some((entry.path.clone(), player));
            }
            if let Some((_, Ok(player))) = &mut self.video {
                player.update(&ctx);
                if player.is_playing() {
                    ctx.request_repaint();
                }
                video_frame = player.texture().map(|t| t.id());
                video_size = player.size().map(|[w, h]| vec2(w as f32, h as f32));
            }
        } else {
            self.video = None;
        }

        // Pick what to draw: the full image, or the thumbnail as an instant placeholder.
        let full = self.full.get(&entry.path);
        let (source_size, status) = if entry.is_video {
            let status = match &self.video {
                Some((_, Err(e))) => format!("error: {e}"),
                Some((_, Ok(p))) if p.failed() => "can't play this video".to_owned(),
                Some((_, Ok(p))) => video::format_time(p.duration()),
                None => String::new(),
            };
            (video_size, status)
        } else {
            match full {
                Some(Full::Ready(img)) => (Some(img.source_size), String::new()),
                Some(Full::Failed(e)) => (None, format!("error: {e}")),
                _ => (None, "loading…".to_owned()),
            }
        };
        let thumb = match self.thumbs.get(&entry.path) {
            Some(Thumb::Ready { tex, .. }) => Some(tex.clone()),
            _ => None,
        };
        let still = match full {
            Some(Full::Ready(img)) if entry.is_video => Some(img.main.clone()),
            _ => None,
        };
        let natural = source_size
            .or_else(|| still.as_ref().map(|t| t.size_vec2()))
            .or_else(|| thumb.as_ref().map(|t| t.size_vec2()));

        // Ctrl+wheel always zooms; the plain wheel follows the setting.
        let (wheel_plain, wheel_ctrl, pinch) = wheel_input(&ctx, resp.hovered());
        let (wheel, wheel_nav) =
            if self.settings.wheel_zooms { (wheel_plain + wheel_ctrl, 0.0) } else { (wheel_ctrl, wheel_plain) };

        let mut zoom_pct = None;
        if let (Some(natural), Some(v)) = (natural, self.viewer.as_mut()) {
            // Fit fills the view (longest side); with "Enlarge small images" off, photos stop at
            // 100% (1 source px = 1 screen px). Videos always fill.
            let fill = (rect.width() / natural.x).min(rect.height() / natural.y);
            let enlarge = entry.is_video || source_size.is_none() || self.settings.enlarge_small;
            let fit = if enlarge { fill } else { fill.min(1.0 / ppp) };
            let factor = wheel_zoom_factor(wheel) * pinch * self.pending_key_zoom;
            let focus = if wheel != 0.0 || pinch != 1.0 { resp.hover_pos().unwrap_or(rect.center()) } else { rect.center() };
            let (draw, scale) = v.view.update(&ctx, rect, &resp, natural, fit, factor, focus, source_size.is_some());
            let size = draw.size();
            zoom_pct = Some(scale * ppp * 100.0);

            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let tex_id = match self.full.get(&entry.path) {
                _ if entry.is_video => video_frame
                    .or_else(|| still.as_ref().map(|t| t.id()))
                    .or_else(|| thumb.as_ref().map(|t| t.id()))
                    .unwrap_or_default(),
                Some(Full::Ready(img)) => {
                    let physical = size * ppp;
                    match &img.screen {
                        _ if !img.frames.is_empty() => img.frame_at(ctx.input(|i| i.time), &ctx).unwrap_or(img.main.id()),
                        Some(s) if physical.x <= s.size_vec2().x * 1.05 => s.id(),
                        _ => img.main.id(),
                    }
                }
                _ => match &thumb {
                    Some(t) => t.id(),
                    None => egui::TextureId::default(),
                },
            };
            let hdr_shape = match self.full.get(&entry.path) {
                Some(Full::Ready(FullImage { hdr: Some(h), .. })) => h.gpu.paint(draw, rect, h.exposure(), h.tonemap),
                _ => None,
            };
            if let Some(shape) = hdr_shape {
                ui.painter().add(shape);
            } else if tex_id != egui::TextureId::default() {
                ui.painter().with_clip_rect(rect).image(tex_id, draw, uv, Color32::WHITE);
            }
            let blur = if entry.is_video {
                match &self.video {
                    Some((_, Ok(p))) => p.backdrop().map(|t| t.id()),
                    _ => None,
                }
            } else {
                match self.full.get(&entry.path) {
                    Some(Full::Ready(img)) => img.blur.as_ref().map(|t| t.id()),
                    _ => None,
                }
            };
            self.backdrop = blur.map(|texture| crate::glass::Backdrop { texture, draw });
        }
        self.pending_key_zoom = 1.0;

        if wheel_nav != 0.0 {
            if let Some(v) = self.viewer.as_mut() {
                v.wheel_accum += wheel_nav;
                if v.wheel_accum.abs() >= 40.0 {
                    let forward = v.wheel_accum < 0.0;
                    v.wheel_accum = 0.0;
                    if let Some(t) = self.step_image(index, forward) {
                        self.open_viewer(t);
                    }
                }
            }
        }

        if let (false, Some(Full::Failed(e))) = (entry.is_video, self.full.get(&entry.path)) {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("Could not open image\n{e}"), FontId::proportional(16.0), theme::p().text_dim);
        }
        let mut menu_action = None;
        let edit_label = self.edit_label();
        resp.context_menu(|ui| file_menu(ui, &edit_label, index, &entry, false, &mut menu_action));
        if menu_action.is_some() {
            self.pending_action = menu_action;
        }
        if !entry.is_video {
            self.exposure_control(ui, rect, &entry.path);
            self.texture_controls(ui, rect, &entry.path);
        }
        // Overlay opacity: videos fade their controls (and the info pill) out when the mouse rests.
        let overlay_alpha = if entry.is_video {
            self.video_controls(ui, rect)
        } else {
            let lift = if self.settings.show_info_bar { 82.0 } else { 38.0 };
            self.animation_controls(ui, rect, &entry.path, lift)
        };

        if !self.settings.show_info_bar || overlay_alpha <= 0.0 {
            return;
        }
        // Floating glass info pill (always dark, it sits on top of the photo).
        let pos_in_folder = self.entries[..=index].iter().filter(|e| !e.is_dir).count();
        let total = self.entries.iter().filter(|e| !e.is_dir).count();
        let mut info = format!("{}    {}/{}", entry.name, pos_in_folder, total);
        if let Some(s) = source_size {
            info += &format!("    {}×{}", s.x as u32, s.y as u32);
        }
        if let Some(Full::Ready(img)) = self.full.get(&entry.path) {
            if let Some(format) = &img.format {
                info += &format!("    {format}");
            }
            if let Some(depth) = img.bit_depth {
                info += &format!("    {}", depth.label());
            }
        }
        info += &format!("    {}", fs::human_size(entry.size));
        if let Some(z) = zoom_pct {
            info += &format!("    {z:.0}%");
        }
        if !status.is_empty() {
            info += &format!("    {status}");
        }
        let mut painter = ui.painter().clone();
        painter.multiply_opacity(overlay_alpha);
        let galley = painter.layout_no_wrap(info, FontId::proportional(13.0), theme::DARK.text);
        let pill = Rect::from_center_size(
            pos2(rect.center().x, rect.max.y - 30.0),
            galley.size() + vec2(28.0, 14.0),
        );
        crate::glass::paint(&painter, pill, 255.0, self.backdrop);
        painter.galley(pill.min + vec2(14.0, 7.0), galley, theme::DARK.text);
    }
}

impl CyclopsApp {
    /// The draggable border between the thumbnail grid and the preview pane.
    fn preview_divider(&mut self, ui: &mut egui::Ui, panel: Rect) {
        let x = panel.min.x;
        let handle = Rect::from_x_y_ranges(x - 4.0..=x + 4.0, panel.y_range());
        let resp = ui.interact(handle, ui.scope_id().with("preview_divider"), Sense::drag());
        if resp.dragged() {
            self.settings.preview_width -= resp.drag_delta().x;
            ui.ctx().request_repaint();
        }
        let active = resp.hovered() || resp.dragged();
        if active {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        }
        let stroke = if active { Stroke::new(2.0, theme::p().accent) } else { Stroke::new(1.0, theme::p().border) };
        ui.painter().vline(x, panel.y_range(), stroke);
    }

    fn preview(&mut self, ui: &mut egui::Ui) {
        let rect = ui.max_rect();
        // Claim the whole panel; panels size themselves from their content.
        ui.expand_to_include_rect(rect);
        let ppp = ui.ctx().pixels_per_point();
        let Some(index) = self.selected.filter(|&i| i < self.entries.len()) else {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Nothing selected", FontId::proportional(14.0), theme::p().text_dim);
            return;
        };
        let entry = self.entries[index].clone();
        let info_h = 64.0;
        let img_rect = Rect::from_min_max(rect.min, pos2(rect.max.x, rect.max.y - info_h));
        let resp = ui.interact(img_rect, ui.scope_id().with("preview"), Sense::click_and_drag());
        let mut menu_action = None;
        let edit_label = self.edit_label();
        resp.context_menu(|ui| file_menu(ui, &edit_label, index, &entry, true, &mut menu_action));
        if menu_action.is_some() {
            self.pending_action = menu_action;
        }
        if self.preview_for.as_ref() != Some(&entry.path) {
            self.preview_for = Some(entry.path.clone());
            self.preview_view = ZoomPan::default();
        }

        let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
        let mut details = Vec::new();
        if entry.is_dir {
            let side = img_rect.width().min(img_rect.height()) * 0.35;
            let icon_rect = Rect::from_center_size(img_rect.center(), vec2(side, side));
            match self.shell_icon(&entry.path, 256) {
                Some(tex) => {
                    ui.painter().image(tex, icon_rect, uv, Color32::WHITE);
                }
                None => icons::paint(ui.painter(), icon_rect, Icon::Folder, theme::p().accent),
            }
            details.push("Folder".to_owned());
        } else {
            // Full image when decoded, the thumbnail (scaled to the final size) until then.
            let now = ui.ctx().input(|i| i.time);
            let full = match self.full.get(&entry.path) {
                Some(Full::Ready(img)) => Some((img.source_size, img.main.clone(), img.screen.clone(), img.frame_at(now, ui.ctx()))),
                _ => None,
            };
            let thumb = match self.thumbs.get(&entry.path) {
                Some(Thumb::Ready { tex, .. }) => Some(tex.clone()),
                _ => None,
            };
            let natural = full.as_ref().map(|f| f.0).or_else(|| thumb.as_ref().map(|t| t.size_vec2()));
            if let Some(natural) = natural {
                let fill = (img_rect.width() / natural.x).min(img_rect.height() / natural.y);
                let fit = if full.is_some() && !self.settings.enlarge_small { fill.min(1.0 / ppp) } else { fill };
                // In the pane the wheel always zooms (there's nothing to scroll).
                let (plain, ctrl, pinch) = wheel_input(ui.ctx(), resp.hovered());
                let factor = wheel_zoom_factor(plain + ctrl) * pinch;
                let focus = resp.hover_pos().unwrap_or(img_rect.center());
                let (draw, _) =
                    self.preview_view.update(ui.ctx(), img_rect, &resp, natural, fit, factor, focus, full.is_some());
                let tex = match &full {
                    Some((.., Some(frame))) => *frame,
                    Some((_, main, screen, None)) => match screen {
                        Some(s) if draw.width() * ppp <= s.size_vec2().x * 1.05 => s.id(),
                        _ => main.id(),
                    },
                    None => thumb.as_ref().map_or(egui::TextureId::default(), |t| t.id()),
                };
                let hdr_shape = match self.full.get(&entry.path) {
                    Some(Full::Ready(FullImage { hdr: Some(h), .. })) => h.gpu.paint(draw, img_rect, h.exposure(), h.tonemap),
                    _ => None,
                };
                match hdr_shape {
                    Some(shape) => {
                        ui.painter().add(shape);
                    }
                    None => {
                        ui.painter().with_clip_rect(img_rect).image(tex, draw, uv, Color32::WHITE);
                    }
                }
                self.backdrop = match self.full.get(&entry.path) {
                    Some(Full::Ready(img)) => img.blur.as_ref().map(|t| crate::glass::Backdrop { texture: t.id(), draw }),
                    _ => None,
                };
            }
            self.exposure_control(ui, img_rect, &entry.path);
            self.texture_controls(ui, img_rect, &entry.path);
            self.animation_controls(ui, img_rect, &entry.path, 30.0);
            if let Some((size, ..)) = &full {
                details.push(format!("{}×{}", size.x as u32, size.y as u32));
                if let Some(Full::Ready(img)) = self.full.get(&entry.path) {
                    details.extend(img.format.clone());
                    details.extend(img.bit_depth.map(|d| d.label()));
                }
            } else if let Some(Full::Failed(e)) = self.full.get(&entry.path) {
                details.push(format!("error: {e}"));
            }
            details.push(fs::human_size(entry.size));
        }
        if !entry.modified_label.is_empty() {
            details.push(entry.modified_label.clone());
        }

        let painter = ui.painter();
        let mut job = LayoutJob::single_section(
            entry.name.clone(),
            TextFormat { font_id: FontId::proportional(14.0), color: theme::p().text_strong, ..Default::default() },
        );
        job.wrap = TextWrapping::truncate_at_width(rect.width());
        let name = painter.layout_job(job);
        let name_pos = pos2(rect.center().x - name.size().x / 2.0, rect.max.y - info_h + 14.0);
        painter.galley(name_pos, name, theme::p().text_strong);
        painter.text(
            pos2(rect.center().x, rect.max.y - info_h + 38.0),
            Align2::CENTER_TOP,
            details.join("  ·  "),
            FontId::proportional(12.0),
            theme::p().text_dim,
        );
    }
}

impl CyclopsApp {
    /// Floating glass transport bar: play/pause, time, seek, volume, mute.
    /// Fades out (with the cursor) once the mouse rests while playing; returns its opacity.
    /// Opacity of a transport bar: fully shown while paused, hovered or after mouse activity,
    /// fading out (and hiding the cursor over `area`) once the mouse rests during playback.
    fn controls_alpha(&mut self, ctx: &egui::Context, bar: Rect, area: Rect, playing: bool) -> f32 {
        const SHOW_FOR: f64 = 1.6;
        const FADE: f64 = 0.45;
        let (now, active, pointer) = ctx.input(|i| {
            (i.time, i.pointer.is_moving() || i.pointer.any_down() || i.pointer.any_pressed(), i.pointer.hover_pos())
        });
        if active {
            self.last_pointer_activity = now;
        }
        let over_bar = pointer.is_some_and(|p| bar.expand(12.0).contains(p));
        let idle = now - self.last_pointer_activity;
        let alpha = if over_bar || !playing { 1.0 } else { (1.0 - ((idle - SHOW_FOR) / FADE).clamp(0.0, 1.0)) as f32 };
        if alpha <= 0.0 {
            if pointer.is_some_and(|p| area.contains(p)) {
                ctx.set_cursor_icon(egui::CursorIcon::None);
            }
        } else if alpha < 1.0 {
            ctx.request_repaint();
        } else if !over_bar && playing {
            ctx.request_repaint_after(std::time::Duration::from_secs_f64((SHOW_FOR - idle).max(0.0)));
        }
        alpha
    }

    /// Play/pause, frame counter and scrub bar for animated GIF/WebP/APNG; returns its opacity.
    fn animation_controls(&mut self, ui: &mut egui::Ui, area: Rect, path: &Path, lift: f32) -> f32 {
        let playing = match self.full.get(path) {
            Some(Full::Ready(img)) if !img.frames.is_empty() => img.paused_at.is_none(),
            _ => return 1.0,
        };
        let ctx = ui.ctx().clone();
        let bar = Rect::from_center_size(pos2(area.center().x, area.max.y - lift), vec2((area.width() - 32.0).min(560.0), 40.0));
        let alpha = self.controls_alpha(&ctx, bar, area, playing);
        if alpha <= 0.0 {
            return 0.0;
        }
        let Some(Full::Ready(img)) = self.full.get_mut(path) else { return alpha };
        let now = ctx.input(|i| i.time);
        let mut painter = ui.painter().clone();
        painter.multiply_opacity(alpha);
        crate::glass::paint(&painter, bar, 14.0, self.backdrop);
        let (fg, cy) = (theme::DARK.text, bar.center().y);

        let play = Rect::from_center_size(pos2(bar.min.x + 24.0, cy), vec2(30.0, 30.0));
        let play_resp = ui.interact(play, ui.scope_id().with(("anim-play", path)), Sense::click());
        if play_resp.hovered() {
            painter.rect_filled(play, CornerRadius::same(8), Color32::from_white_alpha(20));
        }
        icons::paint(&painter, play.shrink(8.0), if playing { Icon::Pause } else { Icon::Play }, if play_resp.hovered() { Color32::WHITE } else { fg });
        if play_resp.clicked() {
            img.toggle_animation(now);
        }

        let (index, _) = img.frame_index(now).unwrap_or((0, 0.0));
        let counter = painter.layout_no_wrap(format!("{} / {}", index + 1, img.frames.len()), FontId::monospace(12.0), fg);
        let counter_x = bar.max.x - 14.0 - counter.size().x;
        painter.galley(pos2(counter_x, cy - counter.size().y / 2.0), counter, fg);

        let track = Rect::from_min_max(pos2(bar.min.x + 48.0, cy - 2.5), pos2(counter_x - 14.0, cy + 2.5));
        if track.width() > 20.0 {
            let total = img.anim_duration();
            let seek = ui.interact(track.expand2(vec2(6.0, 12.0)), ui.scope_id().with(("anim-seek", path)), Sense::click_and_drag());
            if seek.dragged() || seek.clicked() {
                if let Some(p) = seek.interact_pointer_pos() {
                    img.seek_animation(now, ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) * total);
                    ctx.request_repaint();
                }
            }
            let frac = (img.anim_time(now) / total.max(1e-3)).clamp(0.0, 1.0);
            let knob_x = track.min.x + track.width() * frac;
            painter.rect_filled(track, CornerRadius::same(3), Color32::from_white_alpha(40));
            painter.rect_filled(Rect::from_min_max(track.min, pos2(knob_x, track.max.y)), CornerRadius::same(3), theme::DARK.accent);
            painter.circle_filled(pos2(knob_x, cy), if seek.hovered() || seek.dragged() { 7.0 } else { 5.5 }, Color32::WHITE);
        }
        alpha
    }

    fn video_controls(&mut self, ui: &mut egui::Ui, rect: Rect) -> f32 {
        let lift = if self.settings.show_info_bar { 86.0 } else { 40.0 };
        let bar = Rect::from_center_size(pos2(rect.center().x, rect.max.y - lift), vec2((rect.width() - 48.0).min(760.0), 44.0));
        let ctx = ui.ctx().clone();
        let playing = matches!(&self.video, Some((_, Ok(p))) if p.is_playing());
        let alpha = self.controls_alpha(&ctx, bar, rect, playing);
        if alpha <= 0.0 {
            return 0.0;
        }
        let Some((_, Ok(player))) = &mut self.video else { return 1.0 };

        let mut painter = ui.painter().clone();
        painter.multiply_opacity(alpha);
        crate::glass::paint(&painter, bar, 14.0, self.backdrop);
        let fg = theme::DARK.text;
        let cy = bar.center().y;

        let button = |ui: &mut egui::Ui, center: egui::Pos2, salt: &str, icon: Icon| {
            let r = Rect::from_center_size(center, vec2(32.0, 32.0));
            let resp = ui.interact(r, ui.scope_id().with(salt), Sense::click());
            if resp.hovered() {
                painter.rect_filled(r, CornerRadius::same(8), Color32::from_white_alpha(20));
            }
            icons::paint(&painter, r.shrink(8.0), icon, if resp.hovered() { Color32::WHITE } else { fg });
            resp
        };

        let playing = player.is_playing();
        if button(ui, pos2(bar.min.x + 26.0, cy), "video-play", if playing { Icon::Pause } else { Icon::Play }).clicked() {
            player.toggle();
        }
        let muted = player.muted();
        if button(ui, pos2(bar.max.x - 26.0, cy), "video-mute", if muted { Icon::Mute } else { Icon::Volume }).clicked() {
            player.set_muted(!muted);
            self.settings.video_muted = !muted;
        }

        let (pos, dur) = (player.position(), player.duration());
        let time = painter.layout_no_wrap(
            format!("{} / {}", video::format_time(pos), video::format_time(dur)),
            FontId::monospace(12.0),
            fg,
        );
        let time_x = bar.min.x + 50.0;
        painter.galley(pos2(time_x, cy - time.size().y / 2.0), time.clone(), fg);

        // Volume: a short slider left of the mute button; the wheel over it nudges the level.
        let vol_track = Rect::from_min_max(pos2(bar.max.x - 52.0 - 84.0, cy - 2.5), pos2(bar.max.x - 50.0, cy + 2.5));
        let vol = ui.interact(vol_track.expand2(vec2(6.0, 12.0)), ui.scope_id().with("video-volume"), Sense::click_and_drag());
        let mut volume = player.volume();
        let mut volume_changed = false;
        if vol.dragged() || vol.clicked() {
            if let Some(p) = vol.interact_pointer_pos() {
                volume = ((p.x - vol_track.min.x) / vol_track.width()).clamp(0.0, 1.0);
                volume_changed = true;
            }
        }
        if vol.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                volume = (volume + scroll / 600.0).clamp(0.0, 1.0);
                volume_changed = true;
            }
        }
        if volume_changed {
            player.set_volume(volume);
            self.settings.video_volume = volume;
            // Raising the volume while muted unmutes, like most players.
            if player.muted() && volume > 0.0 {
                player.set_muted(false);
                self.settings.video_muted = false;
            }
        }
        let shown = if player.muted() { 0.0 } else { volume };
        let vol_x = vol_track.min.x + vol_track.width() * shown;
        painter.rect_filled(vol_track, CornerRadius::same(3), Color32::from_white_alpha(40));
        painter.rect_filled(Rect::from_min_max(vol_track.min, pos2(vol_x, vol_track.max.y)), CornerRadius::same(3), fg);
        painter.circle_filled(pos2(vol_x, cy), if vol.hovered() || vol.dragged() { 6.5 } else { 5.0 }, Color32::WHITE);
        if vol.hovered() || vol.dragged() {
            painter.text(
                pos2(vol_x, bar.min.y - 8.0),
                Align2::CENTER_BOTTOM,
                format!("{:.0}%", shown * 100.0),
                FontId::proportional(12.0),
                Color32::WHITE,
            );
        }

        let track = Rect::from_min_max(pos2(time_x + time.size().x + 14.0, cy - 2.5), pos2(vol_track.min.x - 22.0, cy + 2.5));
        if track.width() > 20.0 {
            let seek = ui.interact(track.expand2(vec2(6.0, 12.0)), ui.scope_id().with("video-seek"), Sense::click_and_drag());
            if (seek.dragged() || seek.clicked()) && dur > 0.0 {
                if let Some(p) = seek.interact_pointer_pos() {
                    let frac = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
                    player.seek(frac * dur);
                }
            }
            let frac = if dur > 0.0 { (player.position() / dur).clamp(0.0, 1.0) as f32 } else { 0.0 };
            let knob_x = track.min.x + track.width() * frac;
            painter.rect_filled(track, CornerRadius::same(3), Color32::from_white_alpha(40));
            painter.rect_filled(Rect::from_min_max(track.min, pos2(knob_x, track.max.y)), CornerRadius::same(3), theme::DARK.accent);
            let knob_r = if seek.hovered() || seek.dragged() { 7.0 } else { 5.5 };
            painter.circle_filled(pos2(knob_x, cy), knob_r, Color32::WHITE);
        }
        alpha
    }
}

/// A small round "play" marker drawn over video thumbnails.
fn video_badge(painter: &egui::Painter, center: egui::Pos2, radius: f32) {
    painter.circle(center, radius, Color32::from_black_alpha(160), Stroke::new(1.0, Color32::from_white_alpha(60)));
    icons::paint(painter, Rect::from_center_size(center, Vec2::splat(radius * 1.1)), Icon::Play, Color32::WHITE);
}

/// A settings row: title and optional description on the left, the control on the right.
fn setting_row(ui: &mut egui::Ui, title: &str, description: &str, control: impl FnOnce(&mut egui::Ui)) {
    let p = theme::p();
    ui.horizontal(|ui| {
        ui.vertical(|ui| {
            // Leave room for the widest control (three-way segmented) so no row widens the window.
            ui.set_width((ui.available_width() - 290.0).max(160.0));
            ui.spacing_mut().item_spacing.y = 2.0;
            ui.label(egui::RichText::new(title).size(14.0).color(p.text));
            if !description.is_empty() {
                ui.label(egui::RichText::new(description).size(12.0).color(p.text_dim));
            }
        });
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), control);
    });
    ui.add_space(18.0);
}

/// A row of mutually exclusive options in one rounded track. Returns true when changed.
fn segmented<T: PartialEq + Copy>(ui: &mut egui::Ui, value: &mut T, options: &[(T, &str)]) -> bool {
    let p = theme::p();
    let mut changed = false;
    egui::Frame::NONE.fill(p.bg).corner_radius(CornerRadius::same(9)).inner_margin(egui::Margin::same(3)).show(ui, |ui| {
        // Explicit left-to-right: rows place controls right-to-left, which would reverse the options.
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            ui.spacing_mut().item_spacing.x = 2.0;
            for (option, label) in options {
                let selected = *value == *option;
                let text = egui::RichText::new(*label).color(if selected { p.text_strong } else { p.text_dim });
                let button = egui::Button::new(text)
                    .fill(if selected { p.surface_hover } else { Color32::TRANSPARENT })
                    .stroke(if selected { Stroke::new(1.0, p.border) } else { Stroke::NONE })
                    .corner_radius(CornerRadius::same(7))
                    .min_size(vec2(78.0, 28.0));
                if ui.add(button).clicked() && !selected {
                    *value = *option;
                    changed = true;
                }
            }
        });
    });
    changed
}

/// An on/off switch.
fn toggle(ui: &mut egui::Ui, on: &mut bool) {
    let p = theme::p();
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    if resp.clicked() {
        *on = !*on;
    }
    let t = ui.ctx().animate_bool_responsive(resp.id, *on);
    let track = if *on { p.accent } else { p.border };
    ui.painter().rect_filled(rect, CornerRadius::same(11), track);
    let x = egui::lerp((rect.min.x + 11.0)..=(rect.max.x - 11.0), t);
    ui.painter().circle_filled(pos2(x, rect.center().y), 8.0, Color32::WHITE);
}

/// The right-click menu for a file or folder.
fn file_menu(ui: &mut egui::Ui, edit_label: &str, index: usize, entry: &Entry, include_open: bool, action: &mut Option<FileAction>) {
    // In the browser plain letters find files, so editing is Ctrl+E there; the viewer takes E.
    let edit_shortcut = if include_open { "Ctrl+E" } else { "E" };
    let parent = entry.path.parent().map(Path::to_path_buf);
    ui.set_min_width(200.0);
    let mut pick = |a: FileAction, ui: &mut egui::Ui| {
        *action = Some(a);
        ui.close();
    };
    if include_open && ui.add(egui::Button::new("Open").shortcut_text("Enter")).clicked() {
        pick(FileAction::Open(index), ui);
    }
    if !entry.is_dir && ui.add(egui::Button::new(edit_label).shortcut_text(edit_shortcut)).clicked() {
        pick(FileAction::Edit(index), ui);
    }
    if ui.button("Show in Explorer").clicked() {
        pick(FileAction::Reveal(entry.path.clone()), ui);
    }
    if fs::is_virtual(&entry.path) {
        return;
    }
    ui.separator();
    if ui.add(egui::Button::new("Rename…").shortcut_text("F2")).clicked() {
        pick(FileAction::Rename(index), ui);
    }
    if ui.add(egui::Button::new("Delete…").shortcut_text("Del")).clicked() {
        pick(FileAction::Delete(index), ui);
    }
    if let Some(parent) = parent {
        ui.separator();
        if ui.add(egui::Button::new("New folder").shortcut_text("Ctrl+Shift+N")).clicked() {
            pick(FileAction::NewFolder(parent), ui);
        }
    }
}

/// Right-click menu for empty space in the grid (acts on the current folder).
fn folder_menu(ui: &mut egui::Ui, folder: &Path, action: &mut Option<FileAction>) {
    ui.set_min_width(200.0);
    if ui.add(egui::Button::new("New folder").shortcut_text("Ctrl+Shift+N")).clicked() {
        *action = Some(FileAction::NewFolder(folder.to_path_buf()));
        ui.close();
    }
    if ui.button("Show in Explorer").clicked() {
        *action = Some(FileAction::Reveal(folder.to_path_buf()));
        ui.close();
    }
}

impl CyclopsApp {
    fn apply_pending_action(&mut self) {
        let Some(action) = self.pending_action.take() else { return };
        let entry = |i: usize| self.entries.get(i).filter(|e| !fs::is_virtual(&e.path)).cloned();
        match action {
            FileAction::Open(i) => self.activate(i),
            FileAction::Reveal(path) => ops::reveal(&path),
            FileAction::Rename(i) => {
                if let Some(e) = entry(i) {
                    self.dialog = Some(Dialog::Rename { path: e.path, text: e.name, error: None, fresh: true });
                }
            }
            FileAction::Delete(i) => {
                if let Some(e) = entry(i) {
                    self.dialog = Some(Dialog::Delete { path: e.path, name: e.name, is_dir: e.is_dir });
                }
            }
            FileAction::Edit(i) => {
                if let Some(e) = entry(i).filter(|e| !e.is_dir) {
                    if let Err(err) = crate::editor::open(self.settings.editor.as_deref(), &e.path) {
                        self.show_toast(&self.ctx.clone(), err);
                    }
                }
            }
            FileAction::NewFolder(parent) => match ops::new_folder(&parent) {
                Ok(path) => self.folder_created(&parent, path),
                Err(e) => self.show_toast(&self.ctx.clone(), e),
            },
        }
    }

    fn dialogs(&mut self, ctx: &egui::Context) {
        let Some(dialog) = &mut self.dialog else { return };
        let (mut confirm, mut cancel) = (false, false);
        let modal = egui::Modal::new(egui::Id::NULL.with("cyclops-file-dialog")).show(ctx, |ui| {
            ui.set_width(400.0);
            let danger = Color32::from_rgb(220, 60, 60);
            match dialog {
                Dialog::Delete { name, is_dir, .. } => {
                    ui.heading(format!("Delete \u{201c}{name}\u{201d}?"));
                    ui.add_space(4.0);
                    let what = if *is_dir { "This folder and everything in it" } else { "The file" };
                    ui.label(egui::RichText::new(format!("{what} will be moved to the Recycle Bin.")).color(theme::p().text_dim));
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let delete = egui::Button::new(egui::RichText::new("Delete").color(Color32::WHITE)).fill(danger);
                        if ui.add(delete).clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                }
                Dialog::Rename { path, text, error, fresh } => {
                    ui.heading("Rename");
                    ui.add_space(6.0);
                    let output = egui::TextEdit::singleline(text).desired_width(f32::INFINITY).margin(vec2(8.0, 6.0)).show(ui);
                    if *fresh {
                        // Focus the field with the name (not the extension) selected, like Explorer.
                        *fresh = false;
                        output.response.response.request_focus();
                        let total = text.chars().count();
                        let stem = if path.is_dir() {
                            total
                        } else {
                            Path::new(text.as_str()).file_stem().map_or(total, |s| s.to_string_lossy().chars().count())
                        };
                        let mut state = output.state;
                        state.cursor.set_char_range(Some(egui::text::CCursorRange::two(
                            egui::text::CCursor::new(0),
                            egui::text::CCursor::new(stem),
                        )));
                        state.store(ui.ctx(), output.response.response.id);
                    }
                    if let Some(e) = error {
                        ui.add_space(4.0);
                        ui.label(egui::RichText::new(e.as_str()).color(danger));
                    }
                    ui.add_space(12.0);
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Rename").clicked() {
                            confirm = true;
                        }
                        if ui.button("Cancel").clicked() {
                            cancel = true;
                        }
                    });
                }
            }
        });
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            confirm = true;
        }
        if cancel || (modal.should_close() && !confirm) {
            self.dialog = None;
            return;
        }
        if !confirm {
            return;
        }
        match self.dialog.take() {
            Some(Dialog::Delete { path, .. }) => {
                self.release_file(&path);
                match ops::recycle(&path) {
                    Ok(()) => self.remove_entry(&path),
                    Err(e) => self.show_toast(ctx, e),
                }
            }
            Some(Dialog::Rename { path, text, .. }) => {
                self.release_file(&path);
                match ops::rename(&path, &text) {
                    Ok(new_path) => self.renamed_entry(&path, new_path),
                    Err(e) => {
                        self.dialog = Some(Dialog::Rename { path, text, error: Some(e), fresh: false });
                    }
                }
            }
            None => {}
        }
    }

    /// Stop anything holding `path` open (the video player locks its file).
    fn release_file(&mut self, path: &Path) {
        if self.video.as_ref().is_some_and(|(p, _)| p == path) {
            self.video = None;
        }
    }

    fn forget_caches(&mut self, path: &Path) {
        self.thumbs.remove(path);
        self.full.remove(path);
        self.full_lru.retain(|p| p != path);
        if let Some(parent) = path.parent() {
            self.tree_children.remove(parent);
            self.has_subdirs.remove(parent);
        }
    }

    fn remove_entry(&mut self, path: &Path) {
        self.forget_caches(path);
        let Some(idx) = self.entries.iter().position(|e| e.path == path) else { return };
        self.entries.remove(idx);
        let n = self.entries.len();
        self.selected = (n > 0).then(|| idx.min(n - 1));
        if let Some(v) = &self.viewer {
            let current = v.index;
            if current == idx {
                // Show the next image (or the previous one at the end); leave the viewer if none.
                let next = (idx..n).find(|&i| !self.entries[i].is_dir).or_else(|| (0..idx).rev().find(|&i| !self.entries[i].is_dir));
                match next {
                    Some(i) => self.open_viewer(i),
                    None => self.close_viewer(&self.ctx.clone()),
                }
            } else if current > idx {
                if let Some(v) = &mut self.viewer {
                    v.index -= 1;
                }
            }
        }
        self.scroll_to_selected = true;
    }

    fn renamed_entry(&mut self, old: &Path, new: PathBuf) {
        self.forget_caches(old);
        let Some(entry) = self.entries.iter_mut().find(|e| e.path == old) else { return };
        entry.name = fs::file_name(&new);
        entry.path = new.clone();
        let viewing = self.viewer.as_ref().is_some_and(|v| self.entries.get(v.index).is_some_and(|e| e.path == new));
        fs::sort(&mut self.entries, self.sort_key, self.sort_desc);
        let idx = self.entries.iter().position(|e| e.path == new);
        self.selected = idx;
        if let (true, Some(i), Some(v)) = (viewing, idx, self.viewer.as_mut()) {
            v.index = i;
        }
        self.scroll_to_selected = true;
    }

    /// Show a just-created folder (in the grid and tree) and open the rename box for it.
    fn folder_created(&mut self, parent: &Path, path: PathBuf) {
        self.tree_children.remove(parent);
        self.has_subdirs.remove(parent);
        if let Some(root) = self.tree_root.clone() {
            self.expanded.insert((root, parent.to_path_buf()));
        }
        if self.folder.as_deref() == Some(parent) {
            let modified = std::time::SystemTime::now();
            self.entries.push(Entry {
                name: fs::file_name(&path),
                path: path.clone(),
                is_dir: true,
                is_video: false,
                size: 0,
                modified: Some(modified),
                modified_label: fs::format_time(modified),
            });
            fs::sort(&mut self.entries, self.sort_key, self.sort_desc);
            self.selected = self.entries.iter().position(|e| e.path == path);
            self.scroll_to_selected = true;
        }
        let text = fs::file_name(&path);
        self.dialog = Some(Dialog::Rename { path, text, error: None, fresh: true });
    }

    /// The window size/position is restored from last time, which may have been on a bigger or
    /// different display. If it doesn't fit the current monitor, shrink and centre it so the title
    /// bar and edges are reachable.
    fn fit_window_to_screen(&mut self, ctx: &egui::Context) {
        if self.window_checked {
            return;
        }
        let (outer, monitor, maximized, fullscreen) = ctx.input(|i| {
            let v = i.viewport();
            (v.outer_rect, v.monitor_size, v.maximized.unwrap_or(false), v.fullscreen.unwrap_or(false))
        });
        // A fullscreen state restored from last session (closed while viewing an image) has no
        // title bar to grab: always start as a normal window. Fullscreen only comes from the viewer.
        if fullscreen && !self.fullscreen {
            ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(false));
            return; // check the windowed geometry once it has taken effect
        }
        let (Some(outer), Some(monitor)) = (outer, monitor) else { return }; // not known yet
        self.window_checked = true;
        if maximized || fullscreen || monitor.x <= 0.0 || monitor.y <= 0.0 {
            return;
        }
        let screen = Rect::from_min_size(egui::Pos2::ZERO, monitor);
        // Leave room for the taskbar (egui doesn't know the work area, only the monitor size).
        let fits = outer.width() <= monitor.x - 16.0
            && outer.height() <= monitor.y - 56.0
            && screen.contains(outer.min + vec2(40.0, 10.0))
            && outer.max.y <= monitor.y - 40.0;
        if !fits {
            let size = vec2((monitor.x * 0.85).min(outer.width()), (monitor.y * 0.85).min(outer.height()));
            ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(size));
            ctx.send_viewport_cmd(egui::ViewportCommand::OuterPosition(((monitor - size) / 2.0).to_pos2()));
        }
    }

    /// A chrome surface colour: opaque for the Solid material, otherwise translucent so the
    /// Mica/Acrylic backdrop shows through (`opacity` 0–1).
    fn chrome_fill(&self, color: Color32, opacity: f32) -> Color32 {
        if self.settings.material.translucent() {
            Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), (opacity * 255.0) as u8)
        } else {
            color
        }
    }

    fn edit_label(&self) -> String {
        match &self.settings.editor {
            Some(exe) => format!("Edit in {}", crate::editor::name(exe)),
            None => "Edit".to_owned(),
        }
    }

    /// Re-read the current folder and merge changes in place: files edited elsewhere get
    /// fresh thumbnails/images, new files appear, removed ones go. Unchanged thumbnails stay.
    fn sync_folder(&mut self) {
        let Some(folder) = self.folder.clone().filter(|f| !fs::is_remote(f)) else { return };
        let Ok(mut fresh) = fs::list(&folder) else { return };
        let mut changed = false;
        for e in &fresh {
            match self.entries.iter().find(|o| o.path == e.path) {
                Some(old) if old.modified == e.modified && old.size == e.size => {}
                Some(_) => {
                    self.thumbs.remove(&e.path);
                    self.full.remove(&e.path);
                    self.full_lru.retain(|p| p != &e.path);
                    changed = true;
                }
                None => changed = true,
            }
        }
        let gone: Vec<PathBuf> =
            self.entries.iter().filter(|o| !fresh.iter().any(|e| e.path == o.path)).map(|o| o.path.clone()).collect();
        if gone.is_empty() && !changed {
            return;
        }
        for path in &gone {
            self.forget_caches(path);
        }
        let selected = self.selected.and_then(|i| self.entries.get(i)).map(|e| e.path.clone());
        let viewing = self.viewer.as_ref().and_then(|v| self.entries.get(v.index)).map(|e| e.path.clone());
        fs::sort(&mut fresh, self.sort_key, self.sort_desc);
        self.entries = fresh;
        self.selected = selected.and_then(|p| self.entries.iter().position(|e| e.path == p)).or(self.selected.filter(|_| !self.entries.is_empty()).map(|i| i.min(self.entries.len() - 1)));
        if let Some(path) = viewing {
            match self.entries.iter().position(|e| e.path == path) {
                Some(i) => {
                    if let Some(v) = &mut self.viewer {
                        v.index = i;
                    }
                }
                None => self.close_viewer(&self.ctx.clone()),
            }
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context, message: String) {
        self.toast = Some((message, ctx.input(|i| i.time)));
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some((message, since)) = &self.toast else { return };
        let age = ctx.input(|i| i.time) - since;
        if age > 4.0 {
            self.toast = None;
            return;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let alpha = ((4.0 - age) / 0.4).clamp(0.0, 1.0) as f32;
        let mut painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::NULL.with("toast")));
        painter.multiply_opacity(alpha);
        let screen = ctx.content_rect();
        let galley = painter.layout_no_wrap(message.clone(), FontId::proportional(13.0), Color32::WHITE);
        let pill = Rect::from_center_size(pos2(screen.center().x, screen.max.y - 120.0), galley.size() + vec2(32.0, 16.0));
        painter.rect(pill, CornerRadius::same(255), Color32::from_rgba_unmultiplied(150, 30, 30, 230), Stroke::NONE, egui::StrokeKind::Inside);
        painter.galley(pill.min + vec2(16.0, 8.0), galley, Color32::WHITE);
    }

    /// Step a GPU texture's mip level (`dim` 0) or layer/face slice (`dim` 1) by `delta`.
    fn step_sub(&mut self, path: &Path, dim: usize, delta: i32) {
        let Some(Full::Ready(img)) = self.full.get_mut(path) else { return };
        let Some([levels, layers, faces]) = img.sub_layout else { return };
        let [level, layer, face] = img.subs[img.sub_current].coords;
        let target = if dim == 0 {
            let level = (level as i32 + delta).clamp(0, levels as i32 - 1) as u32;
            [level, layer, face]
        } else {
            // Layers and faces step together as one "slice" index, wrapping around.
            let count = (layers * faces) as i32;
            let slice = ((layer * faces + face) as i32 + delta).rem_euclid(count) as u32;
            [level, slice / faces, slice % faces]
        };
        if let Some(i) = img.find_sub(target) {
            img.select_sub(i);
        }
    }

    /// Glass control for GPU textures with mips / layers / faces, in the top-left of `area`.
    fn texture_controls(&mut self, ui: &mut egui::Ui, area: Rect, path: &Path) {
        let (layout, coords, size) = match self.full.get(path) {
            Some(Full::Ready(img)) if !img.subs.is_empty() => {
                (img.sub_layout.unwrap_or([1, 1, 1]), img.subs[img.sub_current].coords, img.source_size)
            }
            _ => return,
        };
        let [levels, layers, faces] = layout;
        let mut rows: Vec<(&str, String, usize)> = Vec::new();
        if levels > 1 {
            rows.push(("Mip", format!("{} / {}   {}×{}", coords[0], levels - 1, size.x as u32, size.y as u32), 0));
        }
        if layers > 1 {
            rows.push(("Layer", format!("{} / {}", coords[1] + 1, layers), 1));
        }
        if faces == 6 {
            rows.push(("Face", CUBE_FACES[coords[2] as usize % 6].to_owned(), 1));
        }
        if rows.is_empty() {
            return;
        }
        let row_h = 30.0;
        let panel = Rect::from_min_size(area.min + vec2(16.0, 16.0), vec2(300.0, rows.len() as f32 * row_h + 12.0));
        let painter = ui.painter().clone();
        crate::glass::paint(&painter, panel, 12.0, self.backdrop);
        let fg = theme::DARK.text;
        let mut step = None;
        for (i, (label, value, dim)) in rows.iter().enumerate() {
            let cy = panel.min.y + 6.0 + row_h * (i as f32 + 0.5);
            painter.text(pos2(panel.min.x + 14.0, cy), Align2::LEFT_CENTER, *label, FontId::proportional(12.5), theme::DARK.text_dim);
            painter.text(pos2(panel.min.x + 92.0, cy), Align2::LEFT_CENTER, value, FontId::monospace(12.0), fg);
            for (dx, delta, icon) in [(-44.0, -1, Icon::ChevronLeft), (-18.0, 1, Icon::ChevronRight)] {
                let r = Rect::from_center_size(pos2(panel.max.x + dx, cy), vec2(24.0, 24.0));
                let resp = ui.interact(r, ui.scope_id().with(("texsub", i, delta)), Sense::click());
                if resp.hovered() {
                    painter.rect_filled(r, CornerRadius::same(6), Color32::from_white_alpha(24));
                }
                let color = if resp.hovered() { Color32::WHITE } else { fg };
                icons::paint(&painter, r.shrink(5.0), icon, color);
                if resp.clicked() {
                    step = Some((*dim, delta));
                }
            }
        }
        if let Some((dim, delta)) = step {
            self.step_sub(path, dim, delta);
        }
    }

    /// Glass exposure slider for HDR/EXR images, at the top of `area`.
    fn exposure_control(&mut self, ui: &mut egui::Ui, area: Rect, path: &Path) {
        // ±8 EV: 16 stops around auto exposure.
        const RANGE: f32 = 8.0;
        let Some(Full::Ready(FullImage { hdr: Some(hdr), .. })) = self.full.get_mut(path) else { return };
        let painter = ui.painter().clone();
        let fg = theme::DARK.text;
        let pill = Rect::from_center_size(pos2(area.center().x, area.min.y + 30.0), vec2(480.0_f32.min(area.width() - 24.0), 36.0));
        crate::glass::paint(&painter, pill, 12.0, self.backdrop);
        let cy = pill.center().y;
        painter.text(pos2(pill.min.x + 14.0, cy), Align2::LEFT_CENTER, "Exposure", FontId::proportional(12.5), fg);

        let track = Rect::from_min_max(pos2(pill.min.x + 88.0, cy - 2.5), pos2(pill.max.x - 124.0, cy + 2.5));
        let resp = ui.interact(track.expand2(vec2(8.0, 12.0)), ui.scope_id().with(("exposure", path)), Sense::click_and_drag());
        if resp.double_clicked() {
            hdr.ev = 0.0;
        } else if resp.dragged() || resp.clicked() {
            if let Some(p) = resp.interact_pointer_pos() {
                let t = ((p.x - track.min.x) / track.width()).clamp(0.0, 1.0);
                // Continuous (no step snapping) so dragging is smooth; a small detent at 0 EV.
                let ev = (t * 2.0 - 1.0) * RANGE;
                hdr.ev = if ev.abs() < 0.06 { 0.0 } else { ev };
            }
        }
        if resp.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                hdr.ev = (hdr.ev + scroll / 120.0).clamp(-RANGE, RANGE);
            }
        }
        let centre_x = track.center().x;
        let knob_x = track.min.x + track.width() * (hdr.ev / RANGE + 1.0) / 2.0;
        painter.rect_filled(track, CornerRadius::same(3), Color32::from_white_alpha(40));
        painter.rect_filled(
            Rect::from_x_y_ranges(centre_x.min(knob_x)..=centre_x.max(knob_x), track.y_range()),
            CornerRadius::same(3),
            theme::DARK.accent,
        );
        painter.vline(centre_x, (cy - 6.0)..=(cy + 6.0), Stroke::new(1.0, Color32::from_white_alpha(90)));
        painter.circle_filled(pos2(knob_x, cy), if resp.hovered() || resp.dragged() { 7.0 } else { 5.5 }, Color32::WHITE);
        painter.text(pos2(track.max.x + 12.0, cy), Align2::LEFT_CENTER, format!("{:+.2} EV", hdr.ev), FontId::monospace(12.0), fg);

        let auto = Rect::from_center_size(pos2(pill.max.x - 28.0, cy), vec2(44.0, 24.0));
        let auto_resp = ui.interact(auto, ui.scope_id().with(("exposure-auto", path)), Sense::click());
        if auto_resp.hovered() {
            painter.rect_filled(auto, CornerRadius::same(6), Color32::from_white_alpha(24));
        }
        painter.text(auto.center(), Align2::CENTER_CENTER, "Auto", FontId::proportional(12.0), if hdr.ev == 0.0 { theme::DARK.text_dim } else { fg });
        if auto_resp.clicked() {
            hdr.ev = 0.0;
        }

    }
}

/// Whether `folder` lives in the tree branch below `root`.
fn under_root(folder: &Path, root: &Path) -> bool {
    if fs::location(root) == fs::Location::Network {
        fs::is_remote(folder)
    } else {
        folder.starts_with(root)
    }
}

impl eframe::App for CyclopsApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.backdrop = None;
        // Native title bar follows the skin (caption matching the toolbar, rounded corners).
        self.fit_window_to_screen(&ctx);
        let dark = ctx.theme() == egui::Theme::Dark;
        let material = self.settings.material;
        if self.chrome_state != Some((dark, material)) {
            let p = theme::p();
            crate::chrome::style(frame, dark, material, p.panel, p.text, p.border);
            self.chrome_state = Some((dark, material));
        }
        crate::perflog::set_enabled(self.settings.perf_log);
        // Coming back to the window (e.g. from the photo editor): pick up changed files.
        let focused = ctx.input(|i| i.viewport().focused.unwrap_or(true));
        if focused && !self.was_focused {
            self.sync_folder();
        }
        self.was_focused = focused;
        self.frame_no += 1;
        self.max_texture_side = ctx.input(|i| i.max_texture_side) as u32;
        if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
            self.screen_side = ((m.x.max(m.y)) * ctx.pixels_per_point()).ceil() as u32;
        }
        self.receive(&ctx);

        // F, F11 and Alt+Enter toggle fullscreen everywhere (unless typing in a text field).
        let in_viewer = self.viewer.is_some();
        let fullscreen_key = self.dialog.is_none()
            && self.settings_page.is_none()
            && !ctx.egui_wants_keyboard_input()
            && ctx.input(|i| {
                i.key_pressed(Key::F11)
                    || (in_viewer && i.key_pressed(Key::F) && i.modifiers.is_none())
                    || (i.modifiers.alt && i.key_pressed(Key::Enter))
            });
        if fullscreen_key {
            self.fullscreen_for_viewer = false;
            self.set_fullscreen(&ctx, !self.fullscreen);
        } else if self.fullscreen && self.viewer.is_none() && ctx.input(|i| i.key_pressed(Key::Escape)) {
            self.set_fullscreen(&ctx, false);
        }
        // While a dialog is open it owns the keyboard (Enter/Esc/Del must not reach the views).
        if self.dialog.is_some() || self.settings_page.is_some() {
        } else if self.viewer.is_some() {
            self.viewer_input(&ctx);
        } else {
            self.grid_keyboard(&ctx);
        }
        self.update_full_cache();
        if self.viewer.is_none() {
            self.video = None;
        }

        if self.viewer.is_some() {
            egui::CentralPanel::no_frame()
                .frame(egui::Frame::NONE.fill(theme::p().viewer_bg))
                .show(ui, |ui| self.viewer(ui));
        } else {
            egui::Panel::top("toolbar")
                .frame(egui::Frame::NONE.fill(self.chrome_fill(theme::p().panel, 0.6)).inner_margin(egui::Margin::symmetric(10, 8)))
                .show(ui, |ui| self.toolbar(ui));
            egui::Panel::bottom("status")
                .frame(egui::Frame::NONE.fill(self.chrome_fill(theme::p().panel, 0.6)).inner_margin(egui::Margin::symmetric(12, 4)))
                .show(ui, |ui| self.status_bar(ui));
            egui::Panel::left("tree")
                .resizable(true)
                .default_size(250.0)
                .size_range(160.0..=500.0)
                .frame(egui::Frame::NONE.fill(self.chrome_fill(theme::p().panel, 0.6)).inner_margin(egui::Margin::symmetric(6, 8)))
                .show(ui, |ui| self.folder_tree(ui));
            // The preview width is owned by us (not egui's panel state) so a drag always sticks.
            let preview = self.settings.three_pane.then(|| {
                let max = (ui.available_width() - 260.0).max(220.0);
                self.settings.preview_width = self.settings.preview_width.clamp(220.0, max);
                egui::Panel::right("preview_pane")
                    .resizable(false)
                    .show_separator_line(false)
                    .exact_size(self.settings.preview_width)
                    .frame(egui::Frame::NONE.fill(self.chrome_fill(theme::p().panel, 0.6)).inner_margin(egui::Margin::same(12)))
                    .show(ui, |ui| self.preview(ui))
                    .response
                    .rect
            });
            egui::CentralPanel::no_frame()
                .frame(egui::Frame::NONE.fill(self.chrome_fill(theme::p().bg, 0.35)))
                .show(ui, |ui| self.grid(ui));
            if let Some(panel) = preview {
                self.preview_divider(ui, panel);
            }
        }
        self.apply_pending_action();
        self.dialogs(&ctx);
        self.settings_window(&ctx);
        self.draw_toast(&ctx);
        self.evict_thumbs();
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if let Some(f) = &self.folder {
            storage.set_string("last_folder", f.display().to_string());
        }
        storage.set_string("thumb_size", self.thumb_size.to_string());
        storage.set_string("sort_key", self.sort_key.label().to_owned());
        storage.set_string("sort_desc", self.sort_desc.to_string());
        storage.set_string("skin", self.settings.skin.label().to_owned());
        storage.set_string("wheel_zooms", self.settings.wheel_zooms.to_string());
        storage.set_string("show_dates", self.settings.show_dates.to_string());
        storage.set_string("show_info_bar", self.settings.show_info_bar.to_string());
        storage.set_string("open_fullscreen", self.settings.open_fullscreen.to_string());
        storage.set_string("enlarge_small", self.settings.enlarge_small.to_string());
        storage.set_string("video_muted", self.settings.video_muted.to_string());
        storage.set_string("video_volume", self.settings.video_volume.to_string());
        storage.set_string("three_pane", self.settings.three_pane.to_string());
        storage.set_string("perf_log", self.settings.perf_log.to_string());
        storage.set_string("material", self.settings.material.label().to_owned());
        storage.set_string("editor", self.settings.editor.as_ref().map(|p| p.display().to_string()).unwrap_or_default());
        storage.set_string("preview_width", self.settings.preview_width.to_string());
        let places: Vec<String> = self.network_places.iter().map(|p| p.display().to_string()).collect();
        storage.set_string("network_places", places.join("\n"));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Fully transparent so the system backdrop shows wherever the UI is translucent.
        if self.settings.material.translucent() { [0.0; 4] } else { theme::p().bg.to_normalized_gamma_f32() }
    }
}
