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
use crate::shell::{self, IconLoader};
use crate::theme;

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

struct Settings {
    skin: Skin,
    /// Viewer mouse wheel zooms (true) or flips between images (false).
    wheel_zooms: bool,
    show_dates: bool,
    show_info_bar: bool,
    /// Folders | thumbnails | preview, instead of folders | thumbnails.
    three_pane: bool,
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
    decode_ms: f32,
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

struct Viewer {
    /// Index into `entries` (always an image).
    index: usize,
    /// Points per source pixel; `None` means fit to window.
    zoom: Option<f32>,
    pan: Vec2,
    /// Running zoom animation, if any.
    anim: Option<ZoomAnim>,
    /// Wheel travel accumulated towards the next image (when the wheel navigates).
    wheel_accum: f32,
}

pub struct CyclopsApp {
    loader: Loader,
    ctx: egui::Context,
    settings: Settings,
    roots: Vec<(String, PathBuf)>,
    tree_children: HashMap<PathBuf, Vec<PathBuf>>,
    /// Remote folders whose children are being enumerated on a background thread.
    children_loading: HashSet<PathBuf>,
    children_tx: Sender<(PathBuf, Vec<PathBuf>)>,
    children_rx: Receiver<(PathBuf, Vec<PathBuf>)>,
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
    pending_key_zoom: f32,

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
            three_pane: flag("three_pane", false),
            preview_width: stored("preview_width").and_then(|s| s.parse().ok()).unwrap_or(460.0),
        };
        theme::install(&cc.egui_ctx);
        cc.egui_ctx.set_theme(settings.skin.preference());
        cc.egui_ctx.options_mut(|o| o.zoom_with_keyboard = false);

        let thumb_size = stored("thumb_size").and_then(|s| s.parse().ok()).unwrap_or(160.0);
        let sort_key = stored("sort_key").and_then(|s| SortKey::from_label(&s)).unwrap_or(SortKey::Name);
        let sort_desc = stored("sort_desc").is_some_and(|s| s == "true");
        let last_folder = stored("last_folder").map(PathBuf::from).filter(|p| p.is_dir() || fs::is_virtual(p));

        let roots = fs::roots();
        let special = roots.iter().map(|(_, p)| p.clone()).collect();
        let (children_tx, children_rx) = unbounded();
        let mut app = Self {
            loader: Loader::new(cc.egui_ctx.clone()),
            ctx: cc.egui_ctx.clone(),
            settings,
            roots,
            tree_children: HashMap::new(),
            children_loading: HashSet::new(),
            children_tx,
            children_rx,
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
            pending_key_zoom: 1.0,
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

    fn shell_icon(&mut self, path: &Path, px: u32) -> Option<egui::TextureId> {
        let wanted = self.special.contains(path)
            || matches!(fs::location(path), fs::Location::Network | fs::Location::Server | fs::Location::ShareRoot);
        if !wanted {
            return None;
        }
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
        self.viewer = Some(Viewer {
            index,
            zoom: None,
            pan: Vec2::ZERO,
            anim: None,
            wheel_accum: 0.0,
        });
        self.selected = Some(index);
    }

    fn close_viewer(&mut self, ctx: &egui::Context) {
        if let Some(v) = self.viewer.take() {
            self.selected = Some(v.index);
            self.scroll_to_selected = true;
        }
        if self.fullscreen {
            self.set_fullscreen(ctx, false);
        }
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
                            Ok(images) => Full::Ready(FullImage {
                                main: ctx.load_texture(name.clone(), images.main, TextureOptions::LINEAR),
                                screen: images
                                    .screen
                                    .map(|s| ctx.load_texture(name + "#screen", s, TextureOptions::LINEAR)),
                                source_size: vec2(images.source_size[0] as f32, images.source_size[1] as f32),
                                decode_ms: images.decode_ms,
                            }),
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
            let slider_w = 490.0;
            let edit = egui::TextEdit::singleline(&mut self.path_edit)
                .desired_width(ui.available_width() - slider_w - 16.0)
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
            ui.add_space(8.0);
            let before = (self.sort_key, self.sort_desc);
            egui::ComboBox::from_id_salt("sort")
                .selected_text(self.sort_key.label())
                .width(130.0)
                .show_ui(ui, |ui| {
                    for key in SortKey::ALL {
                        ui.selectable_value(&mut self.sort_key, key, key.label());
                    }
                });
            let (dir_icon, dir_tip) = if self.sort_desc { (Icon::Down, "Descending") } else { (Icon::Up, "Ascending") };
            if icons::button(ui, dir_icon, true, dir_tip).clicked() {
                self.sort_desc = !self.sort_desc;
            }
            if before != (self.sort_key, self.sort_desc) {
                self.resort();
            }
            ui.add_space(8.0);
            let (r, _) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::hover());
            icons::paint(ui.painter(), r, Icon::Grid, theme::p().text_dim);
            ui.add(egui::Slider::new(&mut self.thumb_size, 80.0..=320.0).show_value(false));
            ui.add_space(8.0);
            if icons::toggle(ui, Icon::TwoPane, !self.settings.three_pane, "Two panes: folders and thumbnails").clicked() {
                self.settings.three_pane = false;
            }
            if icons::toggle(ui, Icon::ThreePane, self.settings.three_pane, "Three panes: folders, thumbnails and preview")
                .clicked()
            {
                self.settings.three_pane = true;
            }
            ui.add_space(4.0);
            let cog = icons::button(ui, Icon::Cog, true, "Settings");
            egui::Popup::menu(&cog)
                .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                .align(egui::RectAlign::BOTTOM_END)
                .gap(6.0)
                .show(|ui| self.settings_menu(ui));
        });
    }

    fn settings_menu(&mut self, ui: &mut egui::Ui) {
        ui.set_width(260.0);
        let heading = |ui: &mut egui::Ui, text: &str| {
            ui.add_space(4.0);
            ui.label(egui::RichText::new(text).small().strong().color(theme::p().text_dim));
        };

        heading(ui, "SKIN");
        ui.horizontal(|ui| {
            let before = self.settings.skin;
            for skin in Skin::ALL {
                ui.selectable_value(&mut self.settings.skin, skin, skin.label());
            }
            if before != self.settings.skin {
                ui.ctx().set_theme(self.settings.skin.preference());
            }
        });

        ui.separator();
        heading(ui, "THUMBNAILS");
        ui.horizontal(|ui| {
            ui.label("Size");
            ui.add(egui::Slider::new(&mut self.thumb_size, 80.0..=320.0).suffix(" px").integer());
        });
        ui.checkbox(&mut self.settings.show_dates, "Show file date");

        ui.separator();
        heading(ui, "VIEWER");
        ui.horizontal(|ui| {
            ui.label("Mouse wheel");
            ui.selectable_value(&mut self.settings.wheel_zooms, true, "Zoom");
            ui.selectable_value(&mut self.settings.wheel_zooms, false, "Next image");
        });
        ui.checkbox(&mut self.settings.show_info_bar, "Show info bar");

        ui.separator();
        ui.label(egui::RichText::new(concat!("Cyclops ", env!("CARGO_PKG_VERSION"))).small().color(theme::p().text_dim));
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

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            if is_current {
                painter.rect_filled(rect, CornerRadius::same(6), theme::p().accent_soft);
            } else if resp.hovered() {
                painter.rect_filled(rect, CornerRadius::same(6), theme::p().surface_hover);
            }
            let chevron_icon = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
            icons::paint(painter, chevron.shrink(2.0), chevron_icon, theme::p().text_dim);
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

        if resp.clicked() && on_chevron || resp.double_clicked() {
            if expanded {
                self.expanded.remove(&key);
            } else {
                self.expanded.insert(key.clone());
            }
        } else if resp.clicked() {
            self.tree_root = Some(root.to_path_buf());
            self.navigate(path.to_path_buf());
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

    fn grid_keyboard(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
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
        let (enter, back, alt_left, refresh) = ctx.input(|i| {
            (
                i.key_pressed(Key::Enter),
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
                if resp.clicked() {
                    clicked = Some(idx);
                }
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

    fn status_bar(&self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let dirs = self.entries.iter().filter(|e| e.is_dir).count();
            let images = self.entries.len() - dirs;
            ui.label(egui::RichText::new(format!("{images} images · {dirs} folders")).color(theme::p().text_dim).small());
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
        let (next, prev, first, last, esc, fs_toggle, fit, one, zoom_in, zoom_out) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowRight) || i.key_pressed(Key::ArrowDown) || i.key_pressed(Key::Space) || i.key_pressed(Key::PageDown),
                i.key_pressed(Key::ArrowLeft) || i.key_pressed(Key::ArrowUp) || i.key_pressed(Key::Backspace) || i.key_pressed(Key::PageUp),
                i.key_pressed(Key::Home),
                i.key_pressed(Key::End),
                i.key_pressed(Key::Escape) || i.key_pressed(Key::Enter),
                i.key_pressed(Key::F11) || i.key_pressed(Key::F),
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
            if self.fullscreen && ctx.input(|i| i.key_pressed(Key::Escape)) {
                self.set_fullscreen(ctx, false);
            } else {
                self.close_viewer(ctx);
            }
            return;
        }
        if fs_toggle {
            self.set_fullscreen(ctx, !self.fullscreen);
        }
        if let Some(v) = &mut self.viewer {
            if fit {
                v.zoom = None;
                v.anim = None;
                v.pan = Vec2::ZERO;
            }
            if one {
                v.zoom = Some(1.0 / ctx.pixels_per_point());
                v.anim = None;
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

        // Pick what to draw: the full image, or the thumbnail as an instant placeholder.
        let full = self.full.get(&entry.path);
        let (source_size, status) = match full {
            Some(Full::Ready(img)) => (Some(img.source_size), format!("{:.0} ms", img.decode_ms)),
            Some(Full::Failed(e)) => (None, format!("error: {e}")),
            _ => (None, "loading…".to_owned()),
        };
        let thumb = match self.thumbs.get(&entry.path) {
            Some(Thumb::Ready { tex, .. }) => Some(tex.clone()),
            _ => None,
        };
        let natural = source_size.or_else(|| thumb.as_ref().map(|t| t.size_vec2()));

        // Wheel input, split by Ctrl: Ctrl+wheel always zooms; the plain wheel follows the setting.
        let (wheel_plain, wheel_ctrl, pinch) = if resp.hovered() {
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
        } else {
            (0.0, 0.0, 1.0)
        };
        let (wheel, wheel_nav) =
            if self.settings.wheel_zooms { (wheel_plain + wheel_ctrl, 0.0) } else { (wheel_ctrl, wheel_plain) };

        let mut zoom_pct = None;
        if let (Some(natural), Some(v)) = (natural, self.viewer.as_mut()) {
            // Fit, but never upscale small images beyond 100% (1 source px = 1 screen px).
            let fit = (rect.width() / natural.x).min(rect.height() / natural.y).min(1.0 / ppp);
            let fit = if source_size.is_some() { fit } else { (rect.width() / natural.x).min(rect.height() / natural.y) };
            let scale = v.zoom.unwrap_or(fit);

            let min_scale = fit * 0.5;
            let max_scale = 32.0 / ppp;

            // Wheel, pinch, keys and double-click start a zoom animation anchored at a focus point.
            let now = ctx.input(|i| i.time);
            let start_zoom = |v: &mut Viewer, to: f32, focus: egui::Pos2, to_fit: bool| {
                let chained = v.anim.is_some();
                v.anim = Some(ZoomAnim {
                    from_scale: scale,
                    to_scale: to,
                    from_pan: v.pan,
                    focus,
                    start: now,
                    duration: if chained { 0.22 } else { 0.32 },
                    chained,
                    to_fit,
                });
            };
            let factor = (wheel * 0.0035).exp() * pinch * self.pending_key_zoom;
            if (factor - 1.0).abs() > 1e-4 && source_size.is_some() {
                let base = v.anim.map_or(scale, |a| a.to_scale);
                let focus = if wheel != 0.0 || pinch != 1.0 {
                    resp.hover_pos().unwrap_or(rect.center())
                } else {
                    rect.center()
                };
                start_zoom(v, (base * factor).clamp(min_scale, max_scale), focus, false);
            }
            if resp.double_clicked() {
                let focus = resp.interact_pointer_pos().unwrap_or(rect.center());
                if v.zoom.is_some() && (scale - fit).abs() > 1e-4 {
                    start_zoom(v, fit, focus, true);
                } else {
                    start_zoom(v, (1.0 / ppp).max(fit * 2.0).min(max_scale), focus, false);
                }
            }
            if resp.drag_started() {
                v.anim = None;
            }

            if let Some(a) = v.anim {
                let (e, done) = a.eased(now);
                // Interpolate in log space so zooming in and out feel equally fast.
                let new_scale = a.from_scale * (a.to_scale / a.from_scale).powf(e);
                let from_center = rect.center() + a.from_pan;
                let new_center = a.focus - (a.focus - from_center) * (new_scale / a.from_scale);
                v.pan = new_center - rect.center();
                if a.to_fit {
                    v.pan *= 1.0 - e;
                }
                v.zoom = Some(new_scale);
                if done {
                    v.anim = None;
                    if a.to_fit {
                        v.zoom = None;
                        v.pan = Vec2::ZERO;
                    }
                } else {
                    ctx.request_repaint();
                }
            }

            // Left-drag pans.
            if resp.dragged() {
                v.pan += resp.drag_delta();
            }

            let scale = v.zoom.unwrap_or(fit);
            let size = natural * scale;
            // Allow panning until an image edge reaches the view centre, so any part of the
            // image (corners included) can be centred, but it can never be lost entirely.
            let max_pan = size / 2.0;
            v.pan = v.pan.clamp(-max_pan, max_pan);
            let draw = Rect::from_center_size(rect.center() + v.pan, size);
            if resp.hovered() {
                ctx.set_cursor_icon(if resp.dragged() { egui::CursorIcon::Grabbing } else { egui::CursorIcon::Grab });
            }
            zoom_pct = Some(scale * ppp * 100.0);

            let uv = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
            let tex_id = match self.full.get(&entry.path) {
                Some(Full::Ready(img)) => {
                    let physical = size * ppp;
                    match &img.screen {
                        Some(s) if physical.x <= s.size_vec2().x * 1.05 => s.id(),
                        _ => img.main.id(),
                    }
                }
                _ => match &thumb {
                    Some(t) => t.id(),
                    None => egui::TextureId::default(),
                },
            };
            if tex_id != egui::TextureId::default() {
                ui.painter().with_clip_rect(rect).image(tex_id, draw, uv, Color32::WHITE);
            }
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

        if let Some(Full::Failed(e)) = self.full.get(&entry.path) {
            ui.painter().text(rect.center(), Align2::CENTER_CENTER, format!("Could not open image\n{e}"), FontId::proportional(16.0), theme::p().text_dim);
        }

        if !self.settings.show_info_bar {
            return;
        }
        // Floating glass info pill (always dark, it sits on top of the photo).
        let pos_in_folder = self.entries[..=index].iter().filter(|e| !e.is_dir).count();
        let total = self.entries.iter().filter(|e| !e.is_dir).count();
        let mut info = format!("{}    {}/{}", entry.name, pos_in_folder, total);
        if let Some(s) = source_size {
            info += &format!("    {}×{}", s.x as u32, s.y as u32);
        }
        info += &format!("    {}", fs::human_size(entry.size));
        if let Some(z) = zoom_pct {
            info += &format!("    {z:.0}%");
        }
        info += &format!("    {status}");
        let painter = ui.painter();
        let galley = painter.layout_no_wrap(info, FontId::proportional(13.0), theme::DARK.text);
        let pill = Rect::from_center_size(
            pos2(rect.center().x, rect.max.y - 30.0),
            galley.size() + vec2(28.0, 14.0),
        );
        painter.rect(pill, CornerRadius::same(255), Color32::from_black_alpha(170), Stroke::new(1.0, Color32::from_white_alpha(18)), egui::StrokeKind::Inside);
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
        let resp = ui.interact(img_rect, ui.scope_id().with("preview"), Sense::click());
        if resp.double_clicked() {
            self.activate(index);
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
            let full = match self.full.get(&entry.path) {
                Some(Full::Ready(img)) => Some((img.source_size, img.main.clone(), img.screen.clone())),
                _ => None,
            };
            let thumb = match self.thumbs.get(&entry.path) {
                Some(Thumb::Ready { tex, .. }) => Some(tex.clone()),
                _ => None,
            };
            let natural = full.as_ref().map(|f| f.0).or_else(|| thumb.as_ref().map(|t| t.size_vec2()));
            if let Some(natural) = natural {
                let mut scale = (img_rect.width() / natural.x).min(img_rect.height() / natural.y);
                if full.is_some() {
                    scale = scale.min(1.0 / ppp);
                }
                let draw = Rect::from_center_size(img_rect.center(), natural * scale);
                let tex = match &full {
                    Some((_, main, screen)) => match screen {
                        Some(s) if draw.width() * ppp <= s.size_vec2().x * 1.05 => s.id(),
                        _ => main.id(),
                    },
                    None => thumb.as_ref().map_or(egui::TextureId::default(), |t| t.id()),
                };
                ui.painter().with_clip_rect(img_rect).image(tex, draw, uv, Color32::WHITE);
            }
            if let Some((size, ..)) = &full {
                details.push(format!("{}×{}", size.x as u32, size.y as u32));
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

/// Whether `folder` lives in the tree branch below `root`.
fn under_root(folder: &Path, root: &Path) -> bool {
    if fs::location(root) == fs::Location::Network {
        fs::is_remote(folder)
    } else {
        folder.starts_with(root)
    }
}

impl eframe::App for CyclopsApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        theme::sync(&ctx);
        self.frame_no += 1;
        self.max_texture_side = ctx.input(|i| i.max_texture_side) as u32;
        if let Some(m) = ctx.input(|i| i.viewport().monitor_size) {
            self.screen_side = ((m.x.max(m.y)) * ctx.pixels_per_point()).ceil() as u32;
        }
        self.receive(&ctx);

        if self.viewer.is_some() {
            self.viewer_input(&ctx);
        } else {
            self.grid_keyboard(&ctx);
        }
        self.update_full_cache();

        if self.viewer.is_some() {
            egui::CentralPanel::no_frame()
                .frame(egui::Frame::NONE.fill(theme::p().viewer_bg))
                .show(ui, |ui| self.viewer(ui));
        } else {
            egui::Panel::top("toolbar")
                .frame(egui::Frame::NONE.fill(theme::p().panel).inner_margin(egui::Margin::symmetric(10, 8)))
                .show(ui, |ui| self.toolbar(ui));
            egui::Panel::bottom("status")
                .frame(egui::Frame::NONE.fill(theme::p().panel).inner_margin(egui::Margin::symmetric(12, 4)))
                .show(ui, |ui| self.status_bar(ui));
            egui::Panel::left("tree")
                .resizable(true)
                .default_size(250.0)
                .size_range(160.0..=500.0)
                .frame(egui::Frame::NONE.fill(theme::p().panel).inner_margin(egui::Margin::symmetric(6, 8)))
                .show(ui, |ui| self.folder_tree(ui));
            // The preview width is owned by us (not egui's panel state) so a drag always sticks.
            let preview = self.settings.three_pane.then(|| {
                let max = (ui.available_width() - 260.0).max(220.0);
                self.settings.preview_width = self.settings.preview_width.clamp(220.0, max);
                egui::Panel::right("preview_pane")
                    .resizable(false)
                    .show_separator_line(false)
                    .exact_size(self.settings.preview_width)
                    .frame(egui::Frame::NONE.fill(theme::p().panel).inner_margin(egui::Margin::same(12)))
                    .show(ui, |ui| self.preview(ui))
                    .response
                    .rect
            });
            egui::CentralPanel::no_frame()
                .frame(egui::Frame::NONE.fill(theme::p().bg))
                .show(ui, |ui| self.grid(ui));
            if let Some(panel) = preview {
                self.preview_divider(ui, panel);
            }
        }
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
        storage.set_string("three_pane", self.settings.three_pane.to_string());
        storage.set_string("preview_width", self.settings.preview_width.to_string());
        let places: Vec<String> = self.network_places.iter().map(|p| p.display().to_string()).collect();
        storage.set_string("network_places", places.join("\n"));
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        theme::p().bg.to_normalized_gamma_f32()
    }
}
