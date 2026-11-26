use crate::{
    flow::{Block, FlowDocument, Inline, LinkTarget, OutlineEntry},
    math::MathCache,
    pdf::{PdfDoc, PdfLinkItem},
    platform::{self, DownloadProgress, InstallationStatus, Release},
    storage::{
        self, Annotation, BookState, EXTENSIONS, Position, Recent, Settings, TabEntry, Workspace,
    },
    theme::{Palette, ThemeMode, smart_invert_rgba},
    word::{Word, WordTextRun},
};
use egui::{
    Align, CentralPanel, Color32, ColorImage, Context, CornerRadius, CursorIcon, FontData,
    FontDefinitions, FontFamily, FontId, Frame, Id, Key, Margin, Panel, PointerButton, Pos2, Rect,
    ResizeDirection, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, Vec2, ViewportCommand,
};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};

enum LoadedSource {
    Pdf(PdfDoc),
    Word(Word),
    Flow(FlowDocument),
}

struct OpenBook {
    id: String,
    title: String,
    digest: String,
    kind: &'static str,
    source: LoadedSource,
    state: BookState,
}

#[derive(Clone, Debug)]
struct PendingJump {
    chapter: usize,
    fraction: f32,
}

struct CachedPageTexture {
    scale_key: u32,
    smart_invert: bool,
    links: Vec<PdfLinkItem>,
    word_runs: Vec<WordTextRun>,
    texture: TextureHandle,
    last_used: u64,
}

#[derive(Hash, PartialEq, Eq, Clone)]
struct MathTexKey {
    latex: String,
    display: bool,
    size_tenths: u32,
    scale_hundredths: u32,
    color: [u8; 4],
}

struct CachedMathTex {
    logical_size: [f32; 2],
    baseline_from_top: f32,
    texture: TextureHandle,
}

struct ImageContextMenu {
    pos: Pos2,
    label: String,
    filename: String,
    png_bytes: Option<Vec<u8>>,
    extra_text: Option<String>,
}

struct LinkWarningModal {
    target: String,
    can_open: bool,
}

struct SearchMatch {
    unit: usize,
    snippet: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarTab {
    Recent,
    Contents,
    Notes,
}

enum BgEvent {
    ReleaseChecked(Result<Option<Release>, String>),
    UpdateFinished(Result<platform::Restart, String>),
}

pub struct HayakuApp {
    data: PathBuf,
    settings: Settings,
    recent: Vec<Recent>,
    workspace: Workspace,
    tabs: Vec<TabEntry>,
    active_book: Option<OpenBook>,
    sidebar_open: bool,
    sidebar_tab: SidebarTab,
    focus_mode: bool,
    show_settings: bool,
    show_search: bool,
    search_query: String,
    search_matches: Vec<SearchMatch>,
    search_cursor: usize,

    fixed_zoom: Option<f32>,
    page_cache: HashMap<(String, usize), CachedPageTexture>,
    use_tick: u64,
    pending_jump: Option<PendingJump>,

    math_cache: MathCache,
    math_textures: HashMap<MathTexKey, Option<CachedMathTex>>,
    image_textures: HashMap<(String, String), Option<TextureHandle>>,

    status_msg: Option<(String, Option<Instant>)>,
    context_menu: Option<ImageContextMenu>,
    link_warning: Option<LinkWarningModal>,
    smart_invert_hint_open: bool,

    install_status: Option<InstallationStatus>,
    show_install_dialog: bool,
    install_selected: HashMap<String, bool>,

    pending_release: Option<Release>,
    show_update_dialog: bool,
    update_progress: Arc<Mutex<Option<DownloadProgress>>>,
    update_downloading: bool,

    bg_rx: mpsc::Receiver<BgEvent>,
    bg_tx: mpsc::Sender<BgEvent>,
    last_inbox_poll: Instant,
    scroll_fraction: f32,
    current_unit: usize,
}

impl HayakuApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data: PathBuf,
        initial: Option<PathBuf>,
        mut dropped: VecDeque<PathBuf>,
    ) -> Self {
        install_system_fonts(&cc.egui_ctx);
        let settings: Settings = storage::read_json(&data.join("settings.json")).unwrap_or_default();
        let recent: Vec<Recent> = storage::read_json(&data.join("recent.json")).unwrap_or_default();
        let workspace: Workspace =
            storage::read_json(&data.join("workspace.json")).unwrap_or_default();
        let install_status = platform::installation_status(&data).ok();
        let mut install_selected = HashMap::new();
        if let Some(st) = &install_status {
            for ext in EXTENSIONS {
                install_selected.insert(
                    (*ext).to_owned(),
                    st.associations.iter().any(|a| a == ext),
                );
            }
        }
        let tabs = if settings.restore_session {
            workspace.tabs.clone()
        } else {
            Vec::new()
        };
        let (bg_tx, bg_rx) = mpsc::channel();
        let mut app = Self {
            data,
            settings,
            recent,
            workspace,
            tabs,
            active_book: None,
            sidebar_open: false,
            sidebar_tab: SidebarTab::Contents,
            focus_mode: false,
            show_settings: false,
            show_search: false,
            search_query: String::new(),
            search_matches: Vec::new(),
            search_cursor: 0,
            fixed_zoom: None,
            page_cache: HashMap::new(),
            use_tick: 0,
            pending_jump: None,
            math_cache: MathCache::default(),
            math_textures: HashMap::new(),
            image_textures: HashMap::new(),
            status_msg: None,
            context_menu: None,
            link_warning: None,
            smart_invert_hint_open: false,
            install_status,
            show_install_dialog: false,
            install_selected,
            pending_release: None,
            show_update_dialog: false,
            update_progress: Arc::new(Mutex::new(None)),
            update_downloading: false,
            bg_rx,
            bg_tx,
            last_inbox_poll: Instant::now(),
            scroll_fraction: 0.0,
            current_unit: 0,
        };

        if let Some(first_path) = initial {
            app.open_path(&first_path);
            while let Some(extra) = dropped.pop_front() {
                app.open_path(&extra);
            }
        } else if app.settings.restore_session
            && let Some(active_id) = app.workspace.active.clone()
            && let Some(rec) = app.recent.iter().find(|r| r.id == active_id).cloned()
            && rec.path.is_file()
        {
            app.open_path(&rec.path);
        }

        if let Some(st) = &app.install_status
            && !st.installation_exists
            && !app.settings.install_dismissed
        {
            app.show_install_dialog = true;
        }
        app.spawn_release_check(false);
        app.maybe_trigger_smart_invert_hint();
        app
    }

    fn theme_mode(&self) -> ThemeMode {
        ThemeMode::from_str_loose(&self.settings.theme)
    }

    fn palette(&self) -> Palette {
        Palette::for_mode(self.theme_mode())
    }

    fn is_smart_invert(&self) -> bool {
        self.theme_mode() == ThemeMode::Dark && self.settings.smart_invert
    }

    fn set_status(&mut self, msg: impl Into<String>, secs: Option<u64>) {
        let deadline = secs.map(|s| Instant::now() + Duration::from_secs(s));
        self.status_msg = Some((msg.into(), deadline));
    }

    fn save_settings(&self) {
        let _ = storage::write_json(&self.data.join("settings.json"), &self.settings);
    }

    fn save_workspace(&mut self) {
        self.workspace.tabs = self.tabs.clone();
        self.workspace.active = self.active_book.as_ref().map(|b| b.id.clone());
        let _ = storage::write_json(&self.data.join("workspace.json"), &self.workspace);
    }

    fn save_active_book(&mut self) {
        if let Some(book) = self.active_book.as_mut() {
            book.state.position = Some(Position {
                index: self.current_unit,
                fraction: self.scroll_fraction,
                anchor: None,
            });
            book.state.digest = book.digest.clone();
            let _ = storage::write_json(&self.data.join(format!("{}.json", book.id)), &book.state);
        }
    }

    fn spawn_release_check(&self, manual: bool) {
        let tx = self.bg_tx.clone();
        std::thread::spawn(move || {
            let res = platform::check_latest_release();
            if manual || matches!(&res, Ok(Some(_))) {
                let _ = tx.send(BgEvent::ReleaseChecked(res));
            }
        });
    }

    fn maybe_trigger_smart_invert_hint(&mut self) {
        if self.active_book.is_some()
            && self.is_smart_invert()
            && !self.settings.smart_invert_hint_seen
            && !self.show_install_dialog
            && !self.show_update_dialog
        {
            self.smart_invert_hint_open = true;
        }
    }

    fn open_path(&mut self, path: &Path) {
        self.save_active_book();
        self.context_menu = None;
        let normalized = match storage::dunce_path(path) {
            Ok(p) => p,
            Err(e) => {
                self.set_status(format!("无法打开文件：{e}"), None);
                return;
            }
        };
        let title = normalized
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "文档".to_owned());
        let digest = match storage::file_digest(&normalized) {
            Ok(d) => d,
            Err(e) => {
                self.set_status(format!("无法读取文件：{e}"), None);
                return;
            }
        };
        let ext = normalized
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let existing_id = self
            .recent
            .iter()
            .find(|r| r.path == normalized || (r.digest == digest && !r.path.exists()))
            .map(|r| r.id.clone());
        let id = existing_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let state: BookState =
            storage::read_json(&self.data.join(format!("{id}.json"))).unwrap_or_default();

        let source_res = match ext.as_str() {
            "pdf" => PdfDoc::open(&normalized).map(LoadedSource::Pdf),
            "doc" | "docx" => Word::open(&normalized).map(LoadedSource::Word),
            "txt" => crate::flow::open_text(&normalized, state.encoding.as_deref())
                .map(LoadedSource::Flow),
            "md" | "markdown" => {
                crate::flow::open_markdown(&normalized).map(LoadedSource::Flow)
            }
            "epub" => crate::flow::open_epub(&normalized).map(LoadedSource::Flow),
            "azw" | "azw3" | "mobi" => {
                crate::flow::open_kindle(&normalized).map(LoadedSource::Flow)
            }
            _ => Err("不支持此文件格式（原生版已移除 HTML 网页解析，支持 PDF/Word/EPUB/Kindle/Markdown/TXT）".to_owned()),
        };

        let source = match source_res {
            Ok(s) => s,
            Err(e) => {
                self.set_status(e, None);
                return;
            }
        };

        let kind = match &source {
            LoadedSource::Pdf(_) => "pdf",
            LoadedSource::Word(_) => "word",
            LoadedSource::Flow(f) => f.kind,
        };

        self.recent.retain(|r| r.id != id);
        self.recent.insert(
            0,
            Recent {
                id: id.clone(),
                path: normalized,
                digest: digest.clone(),
            },
        );
        self.recent.truncate(200);
        let _ = storage::write_json(&self.data.join("recent.json"), &self.recent);

        if !self.tabs.iter().any(|t| t.id == id) {
            self.tabs.push(TabEntry {
                id: id.clone(),
                title: title.clone(),
                last_used: 0,
            });
        }

        let pos = state.position.clone().unwrap_or_default();
        self.current_unit = pos.index;
        self.scroll_fraction = pos.fraction;
        self.pending_jump = if pos.index > 0 || pos.fraction > 0.0 {
            Some(PendingJump {
                chapter: pos.index,
                fraction: pos.fraction,
            })
        } else {
            None
        };

        let has_outline = match &source {
            LoadedSource::Pdf(p) => !p.outline().is_empty(),
            LoadedSource::Word(w) => !w.metadata.outline.is_empty(),
            LoadedSource::Flow(f) => !f.outline.is_empty(),
        };
        if has_outline {
            self.sidebar_open = true;
            self.sidebar_tab = SidebarTab::Contents;
        } else if !state.annotations.is_empty() {
            self.sidebar_open = true;
            self.sidebar_tab = SidebarTab::Notes;
        }

        self.active_book = Some(OpenBook {
            id,
            title,
            digest,
            kind,
            source,
            state,
        });
        self.fixed_zoom = None;
        self.page_cache.clear();
        self.search_matches.clear();
        self.search_cursor = 0;
        self.save_workspace();
        self.set_status("打开成功", Some(2));
        self.maybe_trigger_smart_invert_hint();
    }

    fn close_tab(&mut self, id: &str) {
        if self.active_book.as_ref().is_some_and(|b| b.id == id) {
            self.save_active_book();
            self.active_book = None;
        }
        self.tabs.retain(|t| t.id != id);
        if self.active_book.is_none()
            && let Some(next_id) = self.tabs.last().map(|t| t.id.clone())
            && let Some(rec) = self.recent.iter().find(|r| r.id == next_id).cloned()
        {
            self.open_path(&rec.path);
        } else {
            self.save_workspace();
        }
    }

    fn pick_and_open_files(&mut self) {
        if let Some(files) = rfd::FileDialog::new()
            .add_filter("HayakuReader 文档", EXTENSIONS)
            .pick_files()
        {
            for path in files {
                self.open_path(&path);
            }
        }
    }

    fn handle_link_click(&mut self, target: &LinkTarget) {
        match target {
            LinkTarget::Internal { chapter, .. } => {
                self.pending_jump = Some(PendingJump {
                    chapter: *chapter,
                    fraction: 0.0,
                });
                self.current_unit = *chapter;
            }
            LinkTarget::External(url) => {
                let valid = platform::validate_external_url(url).is_ok();
                self.link_warning = Some(LinkWarningModal {
                    target: url.clone(),
                    can_open: valid,
                });
            }
        }
    }

    fn run_search(&mut self) {
        self.search_matches.clear();
        self.search_cursor = 0;
        let query = self.search_query.trim().to_lowercase();
        if query.is_empty() {
            return;
        }
        let Some(book) = &self.active_book else {
            return;
        };
        match &book.source {
            LoadedSource::Pdf(pdf) => {
                for i in 0..pdf.page_count() {
                    let txt = pdf.page_text(i);
                    if let Some(pos) = txt.to_lowercase().find(&query) {
                        let snippet = extract_snippet(&txt, pos);
                        self.search_matches.push(SearchMatch { unit: i, snippet });
                    }
                }
            }
            LoadedSource::Word(word) => {
                for i in 0..word.metadata.pages.len() {
                    if let Ok(runs) = word.text_runs(i) {
                        let txt: String = runs
                            .into_iter()
                            .map(|r| if r.line_break { format!("\n{}", r.text) } else { r.text })
                            .collect();
                        if let Some(pos) = txt.to_lowercase().find(&query) {
                            let snippet = extract_snippet(&txt, pos);
                            self.search_matches.push(SearchMatch { unit: i, snippet });
                        }
                    }
                }
            }
            LoadedSource::Flow(flow) => {
                for (i, ch) in flow.chapters.iter().enumerate() {
                    let txt = ch.plain_text();
                    if let Some(pos) = txt.to_lowercase().find(&query) {
                        let snippet = extract_snippet(&txt, pos);
                        self.search_matches.push(SearchMatch { unit: i, snippet });
                    }
                }
            }
        }
        if let Some(first) = self.search_matches.first() {
            self.pending_jump = Some(PendingJump {
                chapter: first.unit,
                fraction: 0.0,
            });
            self.current_unit = first.unit;
        }
    }

    fn poll_background(&mut self, ctx: &Context) {
        while let Ok(ev) = self.bg_rx.try_recv() {
            match ev {
                BgEvent::ReleaseChecked(Ok(Some(rel))) => {
                    self.pending_release = Some(rel);
                    self.show_update_dialog = true;
                }
                BgEvent::ReleaseChecked(Ok(None)) => {
                    self.set_status("已是最新版本。", Some(3));
                }
                BgEvent::ReleaseChecked(Err(e)) => {
                    self.set_status(format!("检查更新失败：{e}"), Some(5));
                }
                BgEvent::UpdateFinished(Ok(restart_req)) => {
                    self.update_downloading = false;
                    self.save_active_book();
                    self.save_workspace();
                    if platform::restart(restart_req).is_ok() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                BgEvent::UpdateFinished(Err(e)) => {
                    self.update_downloading = false;
                    self.set_status(format!("更新失败：{e}"), None);
                }
            }
        }
        if self.last_inbox_poll.elapsed() >= Duration::from_millis(450) {
            self.last_inbox_poll = Instant::now();
            if self.data.join("request-restart").is_file() {
                let _ = fs::remove_file(self.data.join("request-restart"));
                self.save_active_book();
                self.save_workspace();
                ctx.send_viewport_cmd(ViewportCommand::Close);
                return;
            }
            let inbox = self.data.join("inbox");
            if let Ok(entries) = fs::read_dir(&inbox) {
                let mut opened_any = false;
                for file in entries.flatten().filter(|e| {
                    e.path().extension().is_some_and(|ext| ext == "json")
                }) {
                    let path = file.path();
                    if let Ok(paths) = storage::read_json::<Vec<PathBuf>>(&path) {
                        let _ = fs::remove_file(&path);
                        for p in paths {
                            self.open_path(&p);
                            opened_any = true;
                        }
                    }
                }
                if opened_any {
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                }
            }
        }
    }

    fn apply_egui_visuals(&self, ctx: &Context) {
        let p = self.palette();
        let mut visuals = if self.theme_mode() == ThemeMode::Dark {
            egui::Visuals::dark()
        } else {
            egui::Visuals::light()
        };
        visuals.panel_fill = p.paper;
        visuals.window_fill = p.surface;
        visuals.extreme_bg_color = p.code_bg;
        visuals.faint_bg_color = p.side;
        visuals.selection.bg_fill = p.selection;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        visuals.widgets.inactive.bg_fill = p.surface;
        visuals.widgets.inactive.weak_bg_fill = p.surface;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
        visuals.widgets.hovered.bg_fill = p.side;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
        ctx.set_visuals(visuals);
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        let modal_open = self.link_warning.is_some()
            || self.smart_invert_hint_open
            || self.show_install_dialog
            || self.show_update_dialog;
        if modal_open {
            return;
        }
        let (open, find, side, close_tab, next_tab, zoom_in, zoom_out, zoom_reset, f11, esc, wheel) =
            ctx.input(|i| {
                let ctrl = i.modifiers.ctrl || i.modifiers.command;
                (
                    ctrl && i.key_pressed(Key::O),
                    ctrl && i.key_pressed(Key::F),
                    ctrl && i.key_pressed(Key::B),
                    ctrl && i.key_pressed(Key::W),
                    ctrl && i.key_pressed(Key::Tab),
                    ctrl && (i.key_pressed(Key::Plus) || i.key_pressed(Key::Equals)),
                    ctrl && i.key_pressed(Key::Minus),
                    ctrl && i.key_pressed(Key::Num0),
                    i.key_pressed(Key::F11),
                    i.key_pressed(Key::Escape),
                    if ctrl { i.smooth_scroll_delta.y } else { 0.0 },
                )
            });

        if open {
            self.pick_and_open_files();
        }
        if find && self.active_book.is_some() {
            self.show_search = !self.show_search;
        }
        if side && self.active_book.is_some() {
            self.sidebar_open = !self.sidebar_open;
        }
        if close_tab && let Some(id) = self.active_book.as_ref().map(|b| b.id.clone()) {
            self.close_tab(&id);
        }
        if next_tab && self.tabs.len() > 1 {
            let cur = self
                .active_book
                .as_ref()
                .and_then(|b| self.tabs.iter().position(|t| t.id == b.id))
                .unwrap_or(0);
            let next_id = self.tabs[(cur + 1) % self.tabs.len()].id.clone();
            if let Some(rec) = self.recent.iter().find(|r| r.id == next_id).cloned() {
                self.open_path(&rec.path);
            }
        }
        if f11 && self.active_book.is_some() {
            self.focus_mode = !self.focus_mode;
        }
        if esc {
            if self.context_menu.is_some() {
                self.context_menu = None;
            } else if self.focus_mode {
                self.focus_mode = false;
            } else if self.show_settings {
                self.show_settings = false;
            } else if self.show_search {
                self.show_search = false;
            }
        }
        if self.active_book.is_some() && (zoom_in || zoom_out || zoom_reset || wheel != 0.0) {
            let is_fixed = self
                .active_book
                .as_ref()
                .is_some_and(|b| matches!(b.source, LoadedSource::Pdf(_) | LoadedSource::Word(_)));
            if is_fixed {
                if zoom_reset {
                    self.fixed_zoom = None;
                } else {
                    let cur = self.fixed_zoom.unwrap_or(1.0);
                    let factor = if zoom_in || wheel > 0.0 { 1.15 } else { 1.0 / 1.15 };
                    self.fixed_zoom = Some((cur * factor).clamp(0.25, 4.5));
                }
            } else {
                if zoom_reset {
                    self.settings.font_size = 19.0;
                } else if zoom_in || wheel > 0.0 {
                    self.settings.font_size = (self.settings.font_size + 1.0).min(34.0);
                } else {
                    self.settings.font_size = (self.settings.font_size - 1.0).max(13.0);
                }
                self.save_settings();
            }
        }
    }

    fn draw_resize_edges(&self, ctx: &Context) {
        if ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let rect = ctx.input(|i| i.viewport_rect());
        let edge = 5.0;
        let corner = 10.0;
        let specs = [
            (
                "resize_nw",
                Rect::from_min_max(rect.min, rect.min + Vec2::splat(corner)),
                ResizeDirection::NorthWest,
                CursorIcon::ResizeNwSe,
            ),
            (
                "resize_ne",
                Rect::from_min_max(
                    Pos2::new(rect.max.x - corner, rect.min.y),
                    Pos2::new(rect.max.x, rect.min.y + corner),
                ),
                ResizeDirection::NorthEast,
                CursorIcon::ResizeNeSw,
            ),
            (
                "resize_sw",
                Rect::from_min_max(
                    Pos2::new(rect.min.x, rect.max.y - corner),
                    Pos2::new(rect.min.x + corner, rect.max.y),
                ),
                ResizeDirection::SouthWest,
                CursorIcon::ResizeNeSw,
            ),
            (
                "resize_se",
                Rect::from_min_max(rect.max - Vec2::splat(corner), rect.max),
                ResizeDirection::SouthEast,
                CursorIcon::ResizeNwSe,
            ),
            (
                "resize_n",
                Rect::from_min_max(
                    Pos2::new(rect.min.x + corner, rect.min.y),
                    Pos2::new(rect.max.x - corner, rect.min.y + edge),
                ),
                ResizeDirection::North,
                CursorIcon::ResizeVertical,
            ),
            (
                "resize_s",
                Rect::from_min_max(
                    Pos2::new(rect.min.x + corner, rect.max.y - edge),
                    Pos2::new(rect.max.x - corner, rect.max.y),
                ),
                ResizeDirection::South,
                CursorIcon::ResizeVertical,
            ),
            (
                "resize_w",
                Rect::from_min_max(
                    Pos2::new(rect.min.x, rect.min.y + corner),
                    Pos2::new(rect.min.x + edge, rect.max.y - corner),
                ),
                ResizeDirection::West,
                CursorIcon::ResizeHorizontal,
            ),
            (
                "resize_e",
                Rect::from_min_max(
                    Pos2::new(rect.max.x - edge, rect.min.y + corner),
                    Pos2::new(rect.max.x, rect.max.y - corner),
                ),
                ResizeDirection::East,
                CursorIcon::ResizeHorizontal,
            ),
        ];
        for (id, r, dir, cursor) in specs {
            egui::Area::new(Id::unique(id))
                .fixed_pos(r.min)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let resp = ui.allocate_rect(r, Sense::drag());
                    if resp.hovered() {
                        ui.ctx().set_cursor_icon(cursor);
                    }
                    if resp.drag_started_by(PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(ViewportCommand::BeginResize(dir));
                    }
                });
        }
    }

    fn draw_top_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        Panel::top("top_toolbar")
            .exact_size(46.0)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin {
                        left: 10,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    })
                    .stroke(Stroke::new(1.0, p.line)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if ui
                        .button("☰")
                        .on_hover_text("切换侧栏 (Ctrl+B)")
                        .clicked()
                    {
                        self.sidebar_open = !self.sidebar_open;
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("打开文件").color(p.accent_ink))
                                .fill(p.accent),
                        )
                        .clicked()
                    {
                        self.pick_and_open_files();
                    }
                    let title = self
                        .active_book
                        .as_ref()
                        .map(|b| b.title.as_str())
                        .unwrap_or("HayakuReader");
                    let controls_reserve = 320.0;
                    let drag_width = (ui.available_width() - controls_reserve).max(40.0);
                    let (drag_rect, drag_resp) = ui.allocate_exact_size(
                        Vec2::new(drag_width, 36.0),
                        Sense::click_and_drag(),
                    );
                    ui.painter().text(
                        drag_rect.left_center() + Vec2::new(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        title,
                        FontId::proportional(14.5),
                        p.ink,
                    );
                    if drag_resp.double_clicked() {
                        ui.ctx()
                            .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                    } else if drag_resp.drag_started_by(PointerButton::Primary) {
                        ui.ctx().send_viewport_cmd(ViewportCommand::StartDrag);
                    }

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        let close_resp = ui
                            .add_sized(
                                [44.0, 44.0],
                                egui::Button::new("✕").frame(false),
                            )
                            .on_hover_text("关闭");
                        if close_resp.hovered() {
                            ui.painter()
                                .rect_filled(close_resp.rect, 0.0, Color32::from_rgb(0xc4, 0x2b, 0x1c));
                            ui.painter().text(
                                close_resp.rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "✕",
                                FontId::proportional(14.0),
                                Color32::WHITE,
                            );
                        }
                        if close_resp.clicked() {
                            self.save_active_book();
                            self.save_workspace();
                            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                        }
                        if ui
                            .add_sized(
                                [44.0, 44.0],
                                egui::Button::new(if maximized { "❐" } else { "□" }).frame(false),
                            )
                            .on_hover_text(if maximized { "还原" } else { "最大化" })
                            .clicked()
                        {
                            ui.ctx()
                                .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        }
                        if ui
                            .add_sized([44.0, 44.0], egui::Button::new("—").frame(false))
                            .on_hover_text("最小化")
                            .clicked()
                        {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                        }
                        ui.separator();
                        if ui.button("阅读设置").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        if self.active_book.is_some() {
                            if ui.button("书签").clicked() {
                                self.add_bookmark();
                            }
                            if ui.button("搜索").clicked() {
                                self.show_search = !self.show_search;
                            }
                            if self
                                .active_book
                                .as_ref()
                                .is_some_and(|b| b.kind == "markdown")
                            {
                                let is_code = self
                                    .active_book
                                    .as_ref()
                                    .and_then(|b| b.state.view_mode.as_deref())
                                    == Some("code");
                                if ui
                                    .button(if is_code { "渲染模式" } else { "代码模式" })
                                    .clicked()
                                    && let Some(b) = self.active_book.as_mut()
                                {
                                    b.state.view_mode = Some(if is_code {
                                        "rendered".to_owned()
                                    } else {
                                        "code".to_owned()
                                    });
                                    let _ = storage::write_json(
                                        &self.data.join(format!("{}.json", b.id)),
                                        &b.state,
                                    );
                                }
                            }
                        }
                    });
                });
            });
    }

    fn add_bookmark(&mut self) {
        let label = if let Some(book) = &self.active_book {
            match &book.source {
                LoadedSource::Pdf(_) | LoadedSource::Word(_) => {
                    format!("第 {} 页", self.current_unit + 1)
                }
                LoadedSource::Flow(f) => f
                    .chapters
                    .get(self.current_unit)
                    .map(|c| c.title.clone())
                    .filter(|t| !t.is_empty())
                    .unwrap_or_else(|| format!("第 {} 章", self.current_unit + 1)),
            }
        } else {
            return;
        };
        if let Some(book) = self.active_book.as_mut() {
            book.state.annotations.push(Annotation {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "bookmark".to_owned(),
                color: "yellow".to_owned(),
                text: label,
                parts: Vec::new(),
                position: Some(Position {
                    index: self.current_unit,
                    fraction: self.scroll_fraction,
                    anchor: None,
                }),
                digest: book.digest.clone(),
                missing: false,
            });
            self.sidebar_open = true;
            self.sidebar_tab = SidebarTab::Notes;
            self.save_active_book();
            self.set_status("已添加书签。", Some(2));
        }
    }

    fn draw_tabs_bar(&mut self, ui: &mut Ui) {
        if self.tabs.is_empty() || self.focus_mode {
            return;
        }
        let p = self.palette();
        Panel::top("document_tabs")
            .exact_size(34.0)
            .frame(
                Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::symmetric(8, 3))
                    .stroke(Stroke::new(1.0, p.line)),
            )
            .show(ui, |ui| {
                ScrollArea::horizontal().show(ui, |ui| {
                    ui.horizontal(|ui| {
                        let active_id = self.active_book.as_ref().map(|b| b.id.clone());
                        let mut switch_to = None;
                        let mut close_id = None;
                        for tab in &self.tabs {
                            let is_active = active_id.as_deref() == Some(tab.id.as_str());
                            let frame = Frame::new()
                                .fill(if is_active { p.surface } else { p.side })
                                .stroke(Stroke::new(
                                    1.0,
                                    if is_active { p.accent } else { p.line },
                                ))
                                .corner_radius(CornerRadius::same(6))
                                .inner_margin(Margin::symmetric(8, 3));
                            frame.show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    let btn = ui.add(
                                        egui::Button::new(truncate_str(&tab.title, 22))
                                            .frame(false),
                                    );
                                    if btn.clicked() && !is_active {
                                        switch_to = Some(tab.id.clone());
                                    }
                                    if btn.middle_clicked() {
                                        close_id = Some(tab.id.clone());
                                    }
                                    if ui.add(egui::Button::new("×").frame(false)).clicked() {
                                        close_id = Some(tab.id.clone());
                                    }
                                });
                            });
                        }
                        if let Some(id) = close_id {
                            self.close_tab(&id);
                        } else if let Some(id) = switch_to
                            && let Some(rec) = self.recent.iter().find(|r| r.id == id).cloned()
                        {
                            self.open_path(&rec.path);
                        }
                    });
                });
            });
    }

    fn draw_sidebar(&mut self, ui: &mut Ui) {
        if !self.sidebar_open || self.focus_mode {
            return;
        }
        let p = self.palette();
        Panel::left("sidebar_panel")
            .resizable(true)
            .default_size(250.0)
            .size_range(180.0..=360.0)
            .frame(
                Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::same(10))
                    .stroke(Stroke::new(1.0, p.line)),
            )
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Recent, "最近阅读");
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Contents, "目录");
                    ui.selectable_value(&mut self.sidebar_tab, SidebarTab::Notes, "批注");
                });
                ui.separator();
                ScrollArea::vertical().show(ui, |ui| match self.sidebar_tab {
                    SidebarTab::Recent => {
                        let mut delete_id = None;
                        let mut open_path = None;
                        if self.recent.is_empty() {
                            ui.label(RichText::new("暂无最近阅读记录").color(p.muted));
                        } else {
                            if ui.button("清空记录").clicked() {
                                let _ = storage::clear_recent_entries(&self.data, &mut self.recent);
                            }
                            ui.add_space(4.0);
                            for item in self.recent.iter().take(30) {
                                let name = item
                                    .path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| item.path.display().to_string());
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(truncate_str(&name, 22))
                                        .on_hover_text(item.path.display().to_string())
                                        .clicked()
                                    {
                                        open_path = Some(item.path.clone());
                                    }
                                    if ui.button("×").clicked() {
                                        delete_id = Some(item.id.clone());
                                    }
                                });
                            }
                        }
                        if let Some(id) = delete_id {
                            let _ = storage::delete_recent_entry(&self.data, &mut self.recent, &id);
                        }
                        if let Some(path) = open_path {
                            self.open_path(&path);
                        }
                    }
                    SidebarTab::Contents => {
                        let entries: Vec<OutlineEntry> = match self.active_book.as_ref().map(|b| &b.source) {
                            Some(LoadedSource::Pdf(pdf)) => pdf.outline().to_vec(),
                            Some(LoadedSource::Word(w)) => w
                                .metadata
                                .outline
                                .iter()
                                .map(|h| OutlineEntry {
                                    title: h.title.clone(),
                                    index: h.index,
                                    fragment: String::new(),
                                })
                                .collect(),
                            Some(LoadedSource::Flow(f)) => f.outline.clone(),
                            None => Vec::new(),
                        };
                        if entries.is_empty() {
                            ui.label(RichText::new("此文档没有目录").color(p.muted));
                        } else {
                            for entry in entries {
                                let active = entry.index == self.current_unit;
                                let text = if active {
                                    RichText::new(&entry.title).strong().color(p.accent)
                                } else {
                                    RichText::new(&entry.title)
                                };
                                if ui.selectable_label(active, text).clicked() {
                                    self.pending_jump = Some(PendingJump {
                                        chapter: entry.index,
                                        fraction: 0.0,
                                    });
                                    self.current_unit = entry.index;
                                }
                            }
                        }
                    }
                    SidebarTab::Notes => {
                        let mut remove_idx = None;
                        let mut jump_to = None;
                        if let Some(book) = &self.active_book {
                            if book.state.annotations.is_empty() {
                                ui.label(RichText::new("暂无书签或批注").color(p.muted));
                            } else {
                                for (idx, ann) in book.state.annotations.iter().enumerate() {
                                    ui.horizontal(|ui| {
                                        if ui.button(&ann.text).clicked()
                                            && let Some(pos) = &ann.position
                                        {
                                            jump_to = Some((pos.index, pos.fraction));
                                        }
                                        if ui.button("×").clicked() {
                                            remove_idx = Some(idx);
                                        }
                                    });
                                }
                            }
                        } else {
                            ui.label(RichText::new("请先打开文档").color(p.muted));
                        }
                        if let Some(idx) = remove_idx
                            && let Some(book) = self.active_book.as_mut()
                            && idx < book.state.annotations.len()
                        {
                            book.state.annotations.remove(idx);
                            self.save_active_book();
                        }
                        if let Some((unit, fraction)) = jump_to {
                            self.pending_jump = Some(PendingJump {
                                chapter: unit,
                                fraction,
                            });
                            self.current_unit = unit;
                        }
                    }
                });
            });
    }

    fn draw_bottom_bar(&mut self, ui: &mut Ui) {
        if self.active_book.is_none() || self.focus_mode {
            return;
        }
        let p = self.palette();
        let dark = self.theme_mode() == ThemeMode::Dark;
        Panel::bottom("status_footer")
            .exact_size(38.0)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(12, 4))
                    .stroke(Stroke::new(1.0, p.line)),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    let loc = if let Some(book) = &self.active_book {
                        match &book.source {
                            LoadedSource::Pdf(pdf) => {
                                format!("第 {} 页 / 共 {} 页", self.current_unit + 1, pdf.page_count())
                            }
                            LoadedSource::Word(w) => {
                                format!(
                                    "第 {} 页 / 共 {} 页",
                                    self.current_unit + 1,
                                    w.metadata.pages.len()
                                )
                            }
                            LoadedSource::Flow(f) => {
                                let ch_title = f
                                    .chapters
                                    .get(self.current_unit)
                                    .map(|c| c.title.as_str())
                                    .filter(|t| !t.is_empty());
                                let pct = (self.scroll_fraction * 100.0).round() as i32;
                                match ch_title {
                                    Some(t) => format!("{t} · {pct}%"),
                                    None => format!("{pct}%"),
                                }
                            }
                        }
                    } else {
                        String::new()
                    };
                    ui.label(RichText::new(loc).color(p.muted));

                    if dark {
                        ui.add_space(8.0);
                        let active = self.settings.smart_invert;
                        let label = if active {
                            "● 已启用智能反色"
                        } else {
                            "○ 已关闭智能反色"
                        };
                        let btn = egui::Button::new(
                            RichText::new(label)
                                .color(if active { p.ink } else { p.muted })
                                .strong(),
                        )
                        .fill(if active {
                            Color32::from_rgba_unmultiplied(
                                p.accent.r(),
                                p.accent.g(),
                                p.accent.b(),
                                40,
                            )
                        } else {
                            p.surface
                        })
                        .stroke(Stroke::new(1.0, if active { p.accent } else { p.line }))
                        .corner_radius(CornerRadius::same(14));
                        if ui.add(btn).clicked() {
                            self.settings.smart_invert = !self.settings.smart_invert;
                            self.page_cache.clear();
                            self.save_settings();
                        }
                    }

                    let is_fixed = self.active_book.as_ref().is_some_and(|b| {
                        matches!(b.source, LoadedSource::Pdf(_) | LoadedSource::Word(_))
                    });
                    if is_fixed {
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("+").clicked() {
                                let cur = self.fixed_zoom.unwrap_or(1.0);
                                self.fixed_zoom = Some((cur * 1.15).min(4.5));
                            }
                            if ui.button("适合页宽").clicked() {
                                self.fixed_zoom = None;
                            }
                            if ui.button("−").clicked() {
                                let cur = self.fixed_zoom.unwrap_or(1.0);
                                self.fixed_zoom = Some((cur / 1.15).max(0.25));
                            }
                            let max_pages = match self.active_book.as_ref().map(|b| &b.source) {
                                Some(LoadedSource::Pdf(pdf)) => pdf.page_count(),
                                Some(LoadedSource::Word(w)) => w.metadata.pages.len(),
                                _ => 1,
                            };
                            if ui.button("›").clicked() && self.current_unit + 1 < max_pages {
                                let next = self.current_unit + 1;
                                self.current_unit = next;
                                self.pending_jump = Some(PendingJump {
                                    chapter: next,
                                    fraction: 0.0,
                                });
                            }
                            if ui.button("‹").clicked() && self.current_unit > 0 {
                                let prev = self.current_unit - 1;
                                self.current_unit = prev;
                                self.pending_jump = Some(PendingJump {
                                    chapter: prev,
                                    fraction: 0.0,
                                });
                            }
                        });
                    }
                });
            });
    }

    fn draw_welcome(&mut self, ui: &mut Ui) {
        let p = self.palette();
        ScrollArea::vertical().show(ui, |ui| {
            ui.vertical_centered(|ui| {
                ui.add_space(36.0);
                ui.label(RichText::new("HayakuReader (Native v2)").size(28.0).strong().color(p.ink));
                ui.add_space(6.0);
                ui.label(
                    RichText::new("轻量原生 GPU 阅读器 · 支持 PDF、Word、EPUB、Kindle (AZW3/MOBI)、Markdown、TXT\n轻小说振假名（Ruby）与学术公式（行内 / 独行）原生高精度排版")
                        .color(p.muted),
                );
                ui.add_space(16.0);
                if ui
                    .add_sized(
                        [140.0, 36.0],
                        egui::Button::new(RichText::new("打开文件").color(p.accent_ink))
                            .fill(p.accent),
                    )
                    .clicked()
                {
                    self.pick_and_open_files();
                }
                if !self.recent.is_empty() {
                    ui.add_space(24.0);
                    Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.line))
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::same(14))
                        .show(ui, |ui| {
                            ui.set_max_width(540.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("最近阅读").strong());
                                ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                    if ui.button("清空记录").clicked() {
                                        let _ = storage::clear_recent_entries(
                                            &self.data,
                                            &mut self.recent,
                                        );
                                    }
                                });
                            });
                            ui.separator();
                            let mut open_target = None;
                            let mut del_target = None;
                            for rec in self.recent.iter().take(12) {
                                let name = rec
                                    .path
                                    .file_name()
                                    .map(|s| s.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| rec.path.display().to_string());
                                ui.horizontal(|ui| {
                                    if ui
                                        .button(truncate_str(&name, 46))
                                        .on_hover_text(rec.path.display().to_string())
                                        .clicked()
                                    {
                                        open_target = Some(rec.path.clone());
                                    }
                                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                                        if ui.button("×").clicked() {
                                            del_target = Some(rec.id.clone());
                                        }
                                    });
                                });
                            }
                            if let Some(id) = del_target {
                                let _ = storage::delete_recent_entry(&self.data, &mut self.recent, &id);
                            }
                            if let Some(path) = open_target {
                                self.open_path(&path);
                            }
                        });
                }
            });
        });
    }

    fn draw_fixed_viewer(&mut self, ui: &mut Ui) {
        let smart_invert = self.is_smart_invert();
        let avail_w = (ui.available_width() - 32.0).max(200.0);
        let ppp = ui.ctx().pixels_per_point();
        let (book_id, is_pdf, page_sizes) = match self.active_book.as_ref() {
            Some(book) => match &book.source {
                LoadedSource::Pdf(pdf) => (book.id.clone(), true, pdf.page_sizes().to_vec()),
                LoadedSource::Word(w) => (book.id.clone(), false, w.metadata.pages.clone()),
                LoadedSource::Flow(_) => return,
            },
            None => return,
        };
        let first_w = page_sizes.first().map_or(595.0, |s| s[0]).max(1.0);
        let base_fit = (avail_w / first_w).clamp(0.25, 3.0);
        let factor = base_fit * self.fixed_zoom.unwrap_or(1.0);
        let gap = 16.0;

        let mut total_h = 24.0;
        let mut page_tops = Vec::with_capacity(page_sizes.len());
        for [w, h] in &page_sizes {
            let disp_w = w * factor;
            let disp_h = h * factor;
            page_tops.push((total_h, disp_w, disp_h));
            total_h += disp_h + gap;
        }

        let mut scroll = ScrollArea::vertical().id_salt("fixed_scroll");
        if let Some(jump) = self.pending_jump.take()
            && let Some(&(top, _, h)) = page_tops.get(jump.chapter)
        {
            scroll = scroll.vertical_scroll_offset((top - 12.0 + h * jump.fraction).max(0.0));
        }

        let mut clicked_external: Option<String> = None;
        let mut clicked_page: Option<usize> = None;
        let mut requested_menu: Option<ImageContextMenu> = None;

        scroll.show_viewport(ui, |ui, viewport| {
            let (rect, _) = ui.allocate_exact_size(
                Vec2::new(avail_w.max(viewport.width()), total_h),
                Sense::hover(),
            );
            let vis_top = viewport.min.y;
            let vis_bot = viewport.max.y;

            let mut first_visible = None;
            let desired_scale = (factor * ppp).clamp(0.5, 4.5);
            let scale_key = (desired_scale * 100.0).round() as u32;

            for (idx, &(top, disp_w, disp_h)) in page_tops.iter().enumerate() {
                let bot = top + disp_h;
                if bot >= vis_top && top <= vis_bot && first_visible.is_none() {
                    first_visible = Some(idx);
                }
                if bot < vis_top - 400.0 || top > vis_bot + 400.0 {
                    continue;
                }

                let page_x = rect.left() + ((rect.width() - disp_w).max(0.0)) * 0.5;
                let page_rect = Rect::from_min_size(
                    Pos2::new(page_x, rect.top() + top),
                    Vec2::new(disp_w, disp_h),
                );

                let cache_key = (book_id.clone(), idx);
                let needs_render = match self.page_cache.get(&cache_key) {
                    Some(c) => c.scale_key != scale_key || c.smart_invert != smart_invert,
                    None => true,
                };
                if needs_render && let Some(book) = self.active_book.as_ref() {
                    match &book.source {
                        LoadedSource::Pdf(pdf) => {
                            if let Ok(mut rendered) = pdf.render_rgba(idx, desired_scale) {
                                if smart_invert {
                                    smart_invert_rgba(
                                        &mut rendered.rgba,
                                        rendered.width,
                                        rendered.height,
                                        &rendered.image_rects_px,
                                    );
                                }
                                let img = ColorImage::from_rgba_unmultiplied(
                                    [rendered.width as usize, rendered.height as usize],
                                    &rendered.rgba,
                                );
                                let tex = ui.ctx().load_texture(
                                    format!("pdf-{}-{idx}-{scale_key}-{smart_invert}", book.id),
                                    img,
                                    TextureOptions::LINEAR,
                                );
                                let links = pdf.page_links(idx);
                                self.use_tick += 1;
                                self.page_cache.insert(
                                    cache_key.clone(),
                                    CachedPageTexture {
                                        scale_key,
                                        smart_invert,
                                        links,
                                        word_runs: Vec::new(),
                                        texture: tex,
                                        last_used: self.use_tick,
                                    },
                                );
                            }
                        }
                        LoadedSource::Word(word) => {
                            if let Ok((w, h, mut rgba)) = word.render_rgba(idx, Some(desired_scale)) {
                                if smart_invert {
                                    let [pw, ph] = word.metadata.pages[idx];
                                    let sx = w as f32 / pw.max(1.0);
                                    let sy = h as f32 / ph.max(1.0);
                                    let rects: Vec<[f32; 4]> = word
                                        .metadata
                                        .images
                                        .get(idx)
                                        .into_iter()
                                        .flatten()
                                        .map(|[ix, iy, iw, ih]| [ix * sx, iy * sy, iw * sx, ih * sy])
                                        .collect();
                                    smart_invert_rgba(&mut rgba, w, h, &rects);
                                }
                                let img = ColorImage::from_rgba_unmultiplied(
                                    [w as usize, h as usize],
                                    &rgba,
                                );
                                let tex = ui.ctx().load_texture(
                                    format!("word-{}-{idx}-{scale_key}-{smart_invert}", book.id),
                                    img,
                                    TextureOptions::LINEAR,
                                );
                                let word_runs = word.text_runs(idx).unwrap_or_default();
                                self.use_tick += 1;
                                self.page_cache.insert(
                                    cache_key.clone(),
                                    CachedPageTexture {
                                        scale_key,
                                        smart_invert,
                                        links: Vec::new(),
                                        word_runs,
                                        texture: tex,
                                        last_used: self.use_tick,
                                    },
                                );
                            }
                        }
                        LoadedSource::Flow(_) => {}
                    }
                }

                // Evict least-recently-used pages if over 12 cached textures
                while self.page_cache.len() > 12 {
                    if let Some(oldest) = self
                        .page_cache
                        .iter()
                        .min_by_key(|(_, v)| v.last_used)
                        .map(|(k, _)| k.clone())
                    {
                        self.page_cache.remove(&oldest);
                    } else {
                        break;
                    }
                }

                ui.painter().rect_stroke(
                    page_rect,
                    0.0,
                    Stroke::new(1.0, Color32::from_black_alpha(30)),
                    StrokeKind::Outside,
                );
                if let Some(cached) = self.page_cache.get_mut(&cache_key) {
                    self.use_tick += 1;
                    cached.last_used = self.use_tick;
                    ui.painter().image(
                        cached.texture.id(),
                        page_rect,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );

                    // Interactive links overlay
                    for link in &cached.links {
                        let lr = Rect::from_min_size(
                            Pos2::new(
                                page_rect.left() + link.rect_pt[0] * factor,
                                page_rect.top() + link.rect_pt[1] * factor,
                            ),
                            Vec2::new(link.rect_pt[2] * factor, link.rect_pt[3] * factor),
                        );
                        let resp = ui.interact(lr, ui.scope_id().with(("pdf-link", idx, lr.min.x as i32, lr.min.y as i32)), Sense::click());
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                            ui.painter().rect_filled(
                                lr,
                                2.0,
                                Color32::from_rgba_unmultiplied(0x28, 0x58, 0x47, 45),
                            );
                        }
                        if resp.clicked() {
                            if let Some(u) = &link.uri {
                                clicked_external = Some(u.clone());
                            } else if let Some(p) = link.dest_page {
                                clicked_page = Some(p);
                            }
                        }
                    }

                    for (ri, run) in cached.word_runs.iter().enumerate() {
                        if let Some(url) = &run.url {
                            let lr = Rect::from_min_size(
                                Pos2::new(
                                    page_rect.left() + run.left * factor,
                                    page_rect.top() + run.top * factor,
                                ),
                                Vec2::new(run.width * factor, (run.size * 1.2) * factor),
                            );
                            let resp = ui.interact(lr, ui.scope_id().with(("wlink", idx, ri)), Sense::click());
                            if resp.hovered() {
                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                ui.painter().rect_filled(
                                    lr,
                                    2.0,
                                    Color32::from_rgba_unmultiplied(0x28, 0x58, 0x47, 45),
                                );
                            }
                            if resp.clicked() {
                                clicked_external = Some(url.clone());
                            }
                        }
                    }

                    let page_resp = ui.interact(
                        page_rect,
                        ui.scope_id().with(("fixed-page-ctx", idx)),
                        Sense::click(),
                    );
                    if page_resp.secondary_clicked()
                        && let Some(pointer) = ui.ctx().pointer_latest_pos()
                    {
                        let px_pt = (pointer.x - page_rect.left()) / factor;
                        let py_pt = (pointer.y - page_rect.top()) / factor;
                        let mut extracted_png = None;
                        let mut kind_label = "复制此页图像";
                        let mut filename = format!("page-{}.png", idx + 1);
                        let mut copy_text = None;
                        if let Some(book) = &self.active_book {
                            match &book.source {
                                LoadedSource::Pdf(pdf) => {
                                    if let Ok(Some(png)) = pdf.extract_image_png(idx, px_pt, py_pt) {
                                        extracted_png = Some(png);
                                        kind_label = "复制图像";
                                        filename = format!("page-{}-image.png", idx + 1);
                                    } else if let Ok(r) = pdf.render_rgba(idx, desired_scale) {
                                        extracted_png = encode_rgba_png(r.width, r.height, &r.rgba);
                                    }
                                    let pt = pdf.page_text(idx);
                                    if !pt.trim().is_empty() {
                                        copy_text = Some(pt);
                                    }
                                }
                                LoadedSource::Word(word) => {
                                    let hit_box = word
                                        .metadata
                                        .images
                                        .get(idx)
                                        .and_then(|boxes| {
                                            boxes
                                                .iter()
                                                .filter(|[x, y, w, h]| {
                                                    *w >= 8.0
                                                        && *h >= 8.0
                                                        && px_pt >= *x
                                                        && px_pt <= *x + *w
                                                        && py_pt >= *y
                                                        && py_pt <= *y + *h
                                                })
                                                .min_by(|a, b| (a[2] * a[3]).total_cmp(&(b[2] * b[3])))
                                                .copied()
                                        });
                                    if let Ok((rw, rh, rgba)) = word.render_rgba(idx, Some(desired_scale)) {
                                        if let Some([bx, by, bw, bh]) = hit_box {
                                            let [pw, ph] = word.metadata.pages[idx];
                                            let sx = rw as f32 / pw.max(1.0);
                                            let sy = rh as f32 / ph.max(1.0);
                                            extracted_png = crop_rgba_png(
                                                rw,
                                                rh,
                                                &rgba,
                                                (bx * sx).floor().max(0.0) as u32,
                                                (by * sy).floor().max(0.0) as u32,
                                                (bw * sx).ceil().max(1.0) as u32,
                                                (bh * sy).ceil().max(1.0) as u32,
                                            );
                                            kind_label = "复制图像";
                                            filename = format!("page-{}-image.png", idx + 1);
                                        } else {
                                            extracted_png = encode_rgba_png(rw, rh, &rgba);
                                        }
                                    }
                                    if let Ok(runs) = word.text_runs(idx) {
                                        let wt: String = runs
                                            .into_iter()
                                            .map(|r| {
                                                if r.line_break {
                                                    format!("\n{}", r.text)
                                                } else {
                                                    r.text
                                                }
                                            })
                                            .collect();
                                        if !wt.trim().is_empty() {
                                            copy_text = Some(wt);
                                        }
                                    }
                                }
                                LoadedSource::Flow(_) => {}
                            }
                        }
                        if let Some(png) = extracted_png {
                            requested_menu = Some(ImageContextMenu {
                                pos: pointer,
                                label: kind_label.to_owned(),
                                filename,
                                png_bytes: Some(png),
                                extra_text: copy_text,
                            });
                        }
                    }
                } else {
                    ui.painter().rect_filled(page_rect, 0.0, Color32::WHITE);
                }
            }
            if let Some(v) = first_visible {
                self.current_unit = v;
            }
            let max_scroll = (total_h - viewport.height()).max(1.0);
            self.scroll_fraction = (vis_top / max_scroll).clamp(0.0, 1.0);
        });

        let _ = is_pdf;
        if let Some(url) = clicked_external {
            self.handle_link_click(&LinkTarget::External(url));
        }
        if let Some(p) = clicked_page {
            self.pending_jump = Some(PendingJump {
                chapter: p,
                fraction: 0.0,
            });
            self.current_unit = p;
        }
        if let Some(menu) = requested_menu {
            self.context_menu = Some(menu);
        }
    }

    fn draw_flow_viewer(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let font_size = self.settings.font_size;
        let line_height = self.settings.line_height;
        let measure = self.settings.text_width.min((ui.available_width() - 48.0).max(260.0));
        let is_code_mode = self
            .active_book
            .as_ref()
            .and_then(|b| b.state.view_mode.as_deref())
            == Some("code");

        let (book_id, chapters, resources) = match self.active_book.as_ref() {
            Some(book) => match &book.source {
                LoadedSource::Flow(flow) => {
                    (book.id.clone(), flow.chapters.clone(), flow.resources.clone())
                }
                _ => return,
            },
            None => return,
        };

        let mut clicked_link: Option<LinkTarget> = None;
        let mut requested_menu: Option<ImageContextMenu> = None;

        ScrollArea::vertical()
            .id_salt("flow_scroll")
            .show(ui, |ui| {
                let total_w = ui.available_width();
                let side_pad = ((total_w - measure) * 0.5).max(16.0);
                Frame::new()
                    .inner_margin(Margin {
                        left: side_pad as i8,
                        right: side_pad as i8,
                        top: 24,
                        bottom: 64,
                    })
                    .show(ui, |ui| {
                        ui.set_max_width(measure);
                        if is_code_mode {
                            for ch in &chapters {
                                for (ln, line) in ch.raw_source.lines().enumerate() {
                                    ui.horizontal(|ui| {
                                        ui.label(
                                            RichText::new(format!("{:>4} ", ln + 1))
                                                .monospace()
                                                .color(p.muted),
                                        );
                                        ui.label(RichText::new(line).monospace().color(p.ink));
                                    });
                                }
                            }
                            return;
                        }
                        let mut ctx = FlowDrawCtx {
                            book_id: &book_id,
                            palette: p,
                            font_size,
                            line_height,
                            max_width: measure,
                            resources: &resources,
                            math_cache: &mut self.math_cache,
                            math_textures: &mut self.math_textures,
                            image_textures: &mut self.image_textures,
                            clicked_link: &mut clicked_link,
                            requested_menu: &mut requested_menu,
                        };
                        for (idx, chapter) in chapters.iter().enumerate() {
                            if idx > 0 {
                                ui.add_space(18.0);
                                ui.separator();
                                ui.add_space(18.0);
                            }
                            for block in &chapter.blocks {
                                draw_flow_block(ui, block, &mut ctx);
                                ui.add_space(font_size * 0.55);
                            }
                        }
                    });
            });

        if let Some(target) = clicked_link {
            self.handle_link_click(&target);
        }
        if let Some(menu) = requested_menu {
            self.context_menu = Some(menu);
        }
    }

    fn draw_popups(&mut self, ctx: &Context) {
        let p = self.palette();

        if let Some((msg, deadline)) = self.status_msg.clone() {
            if deadline.is_some_and(|d| Instant::now() >= d) {
                self.status_msg = None;
            } else {
                egui::Area::new(Id::unique("toast_area"))
                    .anchor(egui::Align2::RIGHT_BOTTOM, Vec2::new(-18.0, -52.0))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        let resp = Frame::new()
                            .fill(p.surface)
                            .stroke(Stroke::new(1.5, p.accent))
                            .corner_radius(CornerRadius::same(8))
                            .inner_margin(Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_max_width(360.0);
                                ui.label(RichText::new(&msg).color(p.ink));
                            })
                            .response;
                        if resp.clicked() {
                            self.status_msg = None;
                        }
                    });
            }
        }

        if let Some(menu) = self.context_menu.take() {
            let mut keep = true;
            let mut copy_bytes: Option<Vec<u8>> = None;
            let mut save_req: Option<(Vec<u8>, String)> = None;
            let mut copy_text: Option<String> = None;
            egui::Area::new(Id::unique("image_context_menu"))
                .fixed_pos(menu.pos)
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    let frame_resp = Frame::new()
                        .fill(p.surface)
                        .stroke(Stroke::new(1.0, p.line))
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(Margin::same(6))
                        .show(ui, |ui| {
                            ui.set_min_width(150.0);
                            if let Some(txt) = &menu.extra_text
                                && ui.button("复制本页文字").clicked()
                            {
                                copy_text = Some(txt.clone());
                                keep = false;
                            }
                            if let Some(bytes) = &menu.png_bytes {
                                if ui.button(&menu.label).clicked() {
                                    copy_bytes = Some(bytes.clone());
                                    keep = false;
                                }
                                let save_lbl = if menu.label == "复制图像" {
                                    "图像另存为…"
                                } else {
                                    "此页另存为图片…"
                                };
                                if ui.button(save_lbl).clicked() {
                                    save_req = Some((bytes.clone(), menu.filename.clone()));
                                    keep = false;
                                }
                            }
                        })
                        .response;
                    if ctx.input(|i| i.pointer.any_pressed())
                        && !frame_resp.rect.contains(
                            ctx.pointer_interact_pos().unwrap_or(Pos2::ZERO),
                        )
                    {
                        keep = false;
                    }
                });
            if let Some(txt) = copy_text {
                ctx.copy_text(txt);
                self.set_status("已复制文字。", Some(2));
            }
            if let Some(bytes) = copy_bytes {
                match platform::set_clipboard_png(&bytes) {
                    Ok(()) => self.set_status("已复制图像。", Some(2)),
                    Err(e) => self.set_status(format!("复制图像失败：{e}"), None),
                }
            }
            if let Some((bytes, fname)) = save_req
                && let Some(path) = rfd::FileDialog::new()
                    .add_filter("PNG 图像", &["png"])
                    .set_file_name(&fname)
                    .save_file()
            {
                match fs::write(&path, &bytes) {
                    Ok(()) => self.set_status("图像已保存。", Some(2)),
                    Err(e) => self.set_status(format!("保存图像失败：{e}"), None),
                }
            }
            if keep {
                self.context_menu = Some(menu);
            }
        }

        if let Some(modal) = self.link_warning.take() {
            let mut keep = true;
            egui::Window::new("外部链接风险提示")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .fixed_size(Vec2::new(440.0, 210.0))
                .show(ctx, |ui| {
                    let msg = if modal.can_open {
                        "即将离开阅读器并在外部浏览器中打开以下链接。外部页面不受 HayakuReader 控制，可能存在钓鱼、恶意软件或隐私风险，请确认来源可信后再继续："
                    } else {
                        "该链接指向外部目标或不支持的协议，为确保安全无法直接在外部打开："
                    };
                    ui.label(msg);
                    ui.add_space(6.0);
                    Frame::new()
                        .fill(p.side)
                        .stroke(Stroke::new(1.0, p.line))
                        .corner_radius(CornerRadius::same(6))
                        .inner_margin(Margin::same(8))
                        .show(ui, |ui| {
                            ui.label(RichText::new(&modal.target).monospace());
                        });
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if ui.button("复制链接").clicked() {
                            ctx.copy_text(modal.target.clone());
                            self.set_status("已复制链接。", Some(2));
                        }
                        if ui.button("取消").clicked() {
                            keep = false;
                        }
                        if modal.can_open
                            && ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("继续打开").color(p.accent_ink),
                                    )
                                    .fill(p.accent),
                                )
                                .clicked()
                        {
                            if let Err(e) = platform::open_external_url(&modal.target) {
                                self.set_status(e, None);
                            }
                            keep = false;
                        }
                    });
                });
            if keep {
                self.link_warning = Some(modal);
            }
        }

        if self.smart_invert_hint_open {
            egui::Window::new("智能反色提示")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_BOTTOM, Vec2::new(0.0, -56.0))
                .fixed_size(Vec2::new(360.0, 140.0))
                .show(ctx, |ui| {
                    ui.label("已自动启用智能反色以适配深色阅读（保留鲜亮色彩与插图，扫描 PDF 整页反色）。如需查看文档原始色彩，可随时点击下方状态栏的「已启用智能反色」切换关闭。");
                    ui.add_space(8.0);
                    if ui
                        .add(
                            egui::Button::new(RichText::new("我知道了").color(p.accent_ink))
                                .fill(p.accent),
                        )
                        .clicked()
                    {
                        self.settings.smart_invert_hint_seen = true;
                        self.smart_invert_hint_open = false;
                        self.save_settings();
                    }
                });
        }

        if self.show_settings {
            let mut close = false;
            let mut changed = false;
            let mut open_install = false;
            let mut check_upd = false;
            egui::Window::new("阅读设置")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-16.0, 52.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("主题");
                        if ui
                            .selectable_label(self.settings.theme != "dark", "浅色")
                            .clicked()
                        {
                            self.settings.theme = "light".to_owned();
                            changed = true;
                        }
                        if ui
                            .selectable_label(self.settings.theme == "dark", "深色")
                            .clicked()
                        {
                            self.settings.theme = "dark".to_owned();
                            changed = true;
                        }
                    });
                    changed |= ui
                        .add(egui::Slider::new(&mut self.settings.font_size, 13.0..=34.0).text("字号"))
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.settings.line_height, 1.3..=2.4)
                                .step_by(0.1)
                                .text("行距"),
                        )
                        .changed();
                    changed |= ui
                        .add(
                            egui::Slider::new(&mut self.settings.text_width, 480.0..=1150.0)
                                .step_by(10.0)
                                .text("阅读宽度"),
                        )
                        .changed();
                    changed |= ui
                        .checkbox(&mut self.settings.restore_session, "启动时恢复标签页")
                        .changed();
                    if self.install_status.as_ref().is_some_and(|s| s.installed) {
                        changed |= ui
                            .checkbox(&mut self.settings.auto_update, "自动安装更新")
                            .changed();
                    }
                    ui.separator();
                    ui.horizontal(|ui| {
                        if ui.button("恢复默认").clicked() {
                            let seen = self.settings.smart_invert_hint_seen;
                            self.settings = Settings {
                                smart_invert_hint_seen: seen,
                                ..Settings::default()
                            };
                            changed = true;
                        }
                        if let Some(st) = &self.install_status {
                            let label = if st.installed { "文件关联" } else { "安装" };
                            if ui.button(label).clicked() {
                                open_install = true;
                            }
                        }
                        if ui.button("检查更新").clicked() {
                            check_upd = true;
                        }
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                    });
                });
            if changed {
                self.page_cache.clear();
                self.save_settings();
                self.maybe_trigger_smart_invert_hint();
            }
            if open_install {
                self.show_install_dialog = true;
            }
            if check_upd {
                self.spawn_release_check(true);
            }
            if close {
                self.show_settings = false;
            }
        }

        if self.show_search {
            let mut close = false;
            let mut trigger_search = false;
            let mut step: isize = 0;
            egui::Window::new("搜索正文")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::RIGHT_TOP, Vec2::new(-16.0, 52.0))
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        let edit = ui.text_edit_singleline(&mut self.search_query);
                        if edit.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)) {
                            trigger_search = true;
                        }
                        if ui.button("查找").clicked() {
                            trigger_search = true;
                        }
                        if !self.search_matches.is_empty() {
                            ui.label(format!(
                                "{}/{}",
                                self.search_cursor + 1,
                                self.search_matches.len()
                            ));
                            if ui.button("↑").clicked() {
                                step = -1;
                            }
                            if ui.button("↓").clicked() {
                                step = 1;
                            }
                        }
                        if ui.button("关闭").clicked() {
                            close = true;
                        }
                    });
                    if let Some(m) = self.search_matches.get(self.search_cursor) {
                        ui.label(RichText::new(&m.snippet).color(p.muted));
                    }
                });
            if trigger_search {
                self.run_search();
            }
            if step != 0 && !self.search_matches.is_empty() {
                let len = self.search_matches.len() as isize;
                self.search_cursor = (self.search_cursor as isize + step).rem_euclid(len) as usize;
                let unit = self.search_matches[self.search_cursor].unit;
                self.pending_jump = Some(PendingJump {
                    chapter: unit,
                    fraction: 0.0,
                });
                self.current_unit = unit;
            }
            if close {
                self.show_search = false;
            }
        }

        if self.show_install_dialog
            && let Some(st) = self.install_status.clone()
        {
            let mut close = false;
            let mut apply = false;
            egui::Window::new(if st.installed {
                "文件关联"
            } else {
                "安装 HayakuReader"
            })
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(st.path.display().to_string());
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    for ext in EXTENSIONS {
                        let checked = self.install_selected.entry((*ext).to_owned()).or_insert(false);
                        ui.checkbox(checked, format!(".{ext}"));
                    }
                });
                ui.label(
                    RichText::new("Windows 已设置过默认应用的格式，仍由系统默认应用设置决定。")
                        .color(p.muted),
                );
                if st.installed && ui.button("打开 Windows 默认应用").clicked() {
                    let _ = platform::open_windows_defaults();
                }
                ui.horizontal(|ui| {
                    if ui.button("取消").clicked() {
                        self.settings.install_dismissed = true;
                        self.save_settings();
                        close = true;
                    }
                    if ui
                        .add(
                            egui::Button::new(RichText::new("确定").color(p.accent_ink))
                                .fill(p.accent),
                        )
                        .clicked()
                    {
                        apply = true;
                    }
                });
            });
            if apply {
                let selected: Vec<String> = EXTENSIONS
                    .iter()
                    .filter(|ext| *self.install_selected.get(**ext).unwrap_or(&false))
                    .map(|ext| (*ext).to_owned())
                    .collect();
                match platform::apply_installation(&self.data, &selected, !st.installed) {
                    Ok(Some(restart_req)) => {
                        self.save_active_book();
                        self.save_workspace();
                        if platform::restart(restart_req).is_ok() {
                            ctx.send_viewport_cmd(ViewportCommand::Close);
                        }
                    }
                    Ok(None) => {
                        self.set_status("已保存文件关联。", Some(2));
                        self.show_install_dialog = false;
                    }
                    Err(e) => {
                        self.set_status(e, None);
                        self.show_install_dialog = false;
                    }
                }
            } else if close {
                self.show_install_dialog = false;
            }
        }

        if self.show_update_dialog
            && let Some(rel) = self.pending_release.clone()
        {
            let mut close = false;
            egui::Window::new(format!("发现新版本 {}", rel.tag_name))
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
                .show(ctx, |ui| {
                    if !platform::installed() {
                        ui.label("当前为便携运行版。如需应用内更新，请先通过「阅读设置 -> 安装」安装到本机，或前往 GitHub 手动下载。");
                        ui.horizontal(|ui| {
                            if ui.button("前往下载").clicked() {
                                let _ = platform::shell_open(&format!(
                                    "{}/releases/latest",
                                    platform::REPO
                                ));
                                close = true;
                            }
                            if ui.button("关闭").clicked() {
                                close = true;
                            }
                        });
                    } else if self.update_downloading {
                        if let Ok(slot) = self.update_progress.lock()
                            && let Some(prog) = slot.as_ref()
                        {
                            let pct = if prog.total > 0 {
                                prog.downloaded as f32 / prog.total as f32
                            } else {
                                0.0
                            };
                            ui.add(egui::ProgressBar::new(pct).text(format!(
                                "{}% · {} KiB/s",
                                (pct * 100.0).round() as i32,
                                prog.bytes_per_second / 1024
                            )));
                        } else {
                            ui.label("正在下载更新…");
                        }
                    } else {
                        ui.label("更新完成后会校验签名与 SHA-256 并自动重启 HayakuReader。");
                        ui.horizontal(|ui| {
                            if ui.button("稍后").clicked() {
                                close = true;
                            }
                            if ui
                                .add(
                                    egui::Button::new(
                                        RichText::new("立即更新").color(p.accent_ink),
                                    )
                                    .fill(p.accent),
                                )
                                .clicked()
                            {
                                self.update_downloading = true;
                                let data = self.data.clone();
                                let slot = self.update_progress.clone();
                                let tx = self.bg_tx.clone();
                                std::thread::spawn(move || {
                                    let res = platform::download_and_stage_update(&data, &rel, &slot);
                                    let _ = tx.send(BgEvent::UpdateFinished(res));
                                });
                            }
                        });
                    }
                });
            if close {
                self.show_update_dialog = false;
            }
        }
    }
}

impl eframe::App for HayakuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_background(&ctx);
        self.apply_egui_visuals(&ctx);

        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        for p in dropped {
            self.open_path(&p);
        }

        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_active_book();
            self.save_workspace();
        }

        self.handle_shortcuts(&ctx);

        if !self.focus_mode {
            self.draw_top_bar(ui);
            self.draw_tabs_bar(ui);
        }
        self.draw_bottom_bar(ui);
        self.draw_sidebar(ui);

        let p = self.palette();
        CentralPanel::default()
            .frame(Frame::new().fill(p.paper))
            .show(ui, |ui| {
                match self.active_book.as_ref().map(|b| &b.source) {
                    None => self.draw_welcome(ui),
                    Some(LoadedSource::Pdf(_) | LoadedSource::Word(_)) => {
                        self.draw_fixed_viewer(ui)
                    }
                    Some(LoadedSource::Flow(_)) => self.draw_flow_viewer(ui),
                }
            });

        self.draw_popups(&ctx);
        self.draw_resize_edges(&ctx);
    }

    fn on_exit(&mut self) {
        self.save_active_book();
        self.save_workspace();
    }
}

struct FlowDrawCtx<'a> {
    book_id: &'a str,
    palette: Palette,
    font_size: f32,
    line_height: f32,
    max_width: f32,
    resources: &'a HashMap<String, Vec<u8>>,
    math_cache: &'a mut MathCache,
    math_textures: &'a mut HashMap<MathTexKey, Option<CachedMathTex>>,
    image_textures: &'a mut HashMap<(String, String), Option<TextureHandle>>,
    clicked_link: &'a mut Option<LinkTarget>,
    requested_menu: &'a mut Option<ImageContextMenu>,
}

enum LineItem {
    Text {
        galley: Arc<egui::Galley>,
        link: Option<LinkTarget>,
        code_bg: bool,
    },
    Ruby {
        base: Arc<egui::Galley>,
        rt: Arc<egui::Galley>,
    },
    Math {
        tex: egui::TextureId,
        size: Vec2,
        baseline_from_top: f32,
    },
}

struct MeasuredLine {
    items: Vec<LineItem>,
    ascent: f32,
    descent: f32,
}

fn flatten_inline_tokens(
    inlines: &[Inline],
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<LinkTarget>,
    out: &mut Vec<FlatToken>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => {
                for tok in split_wrap_tokens(text) {
                    out.push(FlatToken::Text {
                        text: tok,
                        italic,
                        strike,
                        code,
                        link: link.clone(),
                    });
                }
            }
            Inline::Styled {
                bold: _,
                italic: it,
                strike: st,
                code: cd,
                inlines,
            } => flatten_inline_tokens(
                inlines,
                italic || *it,
                strike || *st,
                code || *cd,
                link.clone(),
                out,
            ),
            Inline::Link { target, inlines } => {
                flatten_inline_tokens(
                    inlines,
                    italic,
                    strike,
                    code,
                    Some(target.clone()),
                    out,
                );
            }
            Inline::Ruby { base, annotation } => {
                out.push(FlatToken::Ruby {
                    base: base.clone(),
                    annotation: annotation.clone(),
                });
            }
            Inline::InlineMath { latex } => {
                out.push(FlatToken::Math {
                    latex: latex.clone(),
                });
            }
            Inline::LineBreak => out.push(FlatToken::LineBreak),
        }
    }
}

enum FlatToken {
    Text {
        text: String,
        italic: bool,
        strike: bool,
        code: bool,
        link: Option<LinkTarget>,
    },
    Ruby {
        base: String,
        annotation: String,
    },
    Math {
        latex: String,
    },
    LineBreak,
}

fn split_wrap_tokens(text: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut ascii_buf = String::new();
    for ch in text.chars() {
        if ch == '\n' {
            if !ascii_buf.is_empty() {
                tokens.push(std::mem::take(&mut ascii_buf));
            }
            tokens.push("\n".to_owned());
        } else if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '\'' | '.' | ',') {
            ascii_buf.push(ch);
        } else {
            if !ascii_buf.is_empty() {
                tokens.push(std::mem::take(&mut ascii_buf));
            }
            if ch == ' ' {
                if let Some(last) = tokens.last_mut()
                    && !last.ends_with(' ')
                    && !last.ends_with('\n')
                {
                    last.push(' ');
                } else {
                    tokens.push(" ".to_owned());
                }
            } else {
                tokens.push(ch.to_string());
            }
        }
    }
    if !ascii_buf.is_empty() {
        tokens.push(ascii_buf);
    }
    tokens
}

fn get_or_load_math(
    ctx_ui: &Ui,
    latex: &str,
    display: bool,
    font_size: f32,
    ink: Color32,
    math_cache: &mut MathCache,
    textures: &mut HashMap<MathTexKey, Option<CachedMathTex>>,
) -> Option<(egui::TextureId, Vec2, f32)> {
    let ppp = ctx_ui.ctx().pixels_per_point();
    let color = [ink.r(), ink.g(), ink.b(), ink.a()];
    let key = MathTexKey {
        latex: latex.trim().to_owned(),
        display,
        size_tenths: (font_size * 10.0).round() as u32,
        scale_hundredths: (ppp * 100.0).round() as u32,
        color,
    };
    if let Some(entry) = textures.get(&key) {
        return entry
            .as_ref()
            .map(|c| (c.texture.id(), Vec2::from(c.logical_size), c.baseline_from_top));
    }
    let rendered = math_cache.render(latex, display, font_size, ppp, color);
    let cached = rendered.map(|r| {
        let img = ColorImage::from_rgba_unmultiplied(
            [r.pixel_width as usize, r.pixel_height as usize],
            &r.rgba,
        );
        let tex = ctx_ui.ctx().load_texture(
            format!("math-{display}-{}-{}", textures.len(), key.latex.len()),
            img,
            TextureOptions::LINEAR,
        );
        CachedMathTex {
            logical_size: r.logical_size,
            baseline_from_top: r.baseline_from_top,
            texture: tex,
        }
    });
    let out = cached
        .as_ref()
        .map(|c| (c.texture.id(), Vec2::from(c.logical_size), c.baseline_from_top));
    textures.insert(key, cached);
    out
}

fn draw_inline_flow(
    ui: &mut Ui,
    inlines: &[Inline],
    scale_mul: f32,
    _force_bold: bool,
    ctx: &mut FlowDrawCtx<'_>,
) {
    let fsize = ctx.font_size * scale_mul;
    let ppp = ui.ctx().pixels_per_point();
    let mut tokens = Vec::new();
    flatten_inline_tokens(inlines, false, false, false, None, &mut tokens);

    let max_w = ui.available_width().min(ctx.max_width).max(60.0);
    let base_ascent = fsize * 0.92;
    let base_descent = fsize * 0.28;
    let rt_size = (fsize * 0.48).max(9.0);

    let mut lines: Vec<MeasuredLine> = vec![MeasuredLine {
        items: Vec::new(),
        ascent: base_ascent,
        descent: base_descent,
    }];
    let mut cur_x = 0.0f32;

    for token in tokens {
        match token {
            FlatToken::LineBreak => {
                lines.push(MeasuredLine {
                    items: Vec::new(),
                    ascent: base_ascent,
                    descent: base_descent,
                });
                cur_x = 0.0;
            }
            FlatToken::Text {
                text,
                italic,
                strike,
                code,
                link,
            } => {
                if text == "\n" {
                    lines.push(MeasuredLine {
                        items: Vec::new(),
                        ascent: base_ascent,
                        descent: base_descent,
                    });
                    cur_x = 0.0;
                    continue;
                }
                let color = if link.is_some() {
                    ctx.palette.accent
                } else {
                    ctx.palette.ink
                };
                let fid = if code {
                    FontId::monospace(fsize * 0.92)
                } else {
                    FontId::proportional(fsize)
                };
                let mut job = egui::text::LayoutJob::default();
                job.append(
                    &text,
                    0.0,
                    egui::TextFormat {
                        font_id: fid,
                        color,
                        italics: italic,
                        underline: if link.is_some() {
                            Stroke::new(1.0, ctx.palette.accent)
                        } else {
                            Stroke::NONE
                        },
                        strikethrough: if strike {
                            Stroke::new(1.0, color)
                        } else {
                            Stroke::NONE
                        },
                        ..Default::default()
                    },
                );
                let galley = ui.fonts_mut(|f| f.layout_job(job));
                let w = galley.rect.width();
                if cur_x > 0.0 && cur_x + w > max_w {
                    lines.push(MeasuredLine {
                        items: Vec::new(),
                        ascent: base_ascent,
                        descent: base_descent,
                    });
                    cur_x = 0.0;
                }
                if let Some(line) = lines.last_mut() {
                    line.items.push(LineItem::Text {
                        galley,
                        link,
                        code_bg: code,
                    });
                    cur_x += w;
                }
            }
            FlatToken::Ruby { base, annotation } => {
                let base_galley = ui.fonts_mut(|f| {
                    f.layout_no_wrap(base, FontId::proportional(fsize), ctx.palette.ink)
                });
                let rt_galley = ui.fonts_mut(|f| {
                    f.layout_no_wrap(annotation, FontId::proportional(rt_size), ctx.palette.muted)
                });
                let atom_w = base_galley.rect.width().max(rt_galley.rect.width()) + 2.0;
                let ruby_ascent = base_ascent + rt_galley.rect.height() + 2.0;
                if cur_x > 0.0 && cur_x + atom_w > max_w {
                    lines.push(MeasuredLine {
                        items: Vec::new(),
                        ascent: base_ascent,
                        descent: base_descent,
                    });
                    cur_x = 0.0;
                }
                if let Some(line) = lines.last_mut() {
                    line.ascent = line.ascent.max(ruby_ascent);
                    line.items.push(LineItem::Ruby {
                        base: base_galley,
                        rt: rt_galley,
                    });
                    cur_x += atom_w;
                }
            }
            FlatToken::Math { latex } => {
                if let Some((tex, size, baseline_from_top)) = get_or_load_math(
                    ui,
                    &latex,
                    false,
                    fsize,
                    ctx.palette.ink,
                    ctx.math_cache,
                    ctx.math_textures,
                ) {
                    if cur_x > 0.0 && cur_x + size.x > max_w {
                        lines.push(MeasuredLine {
                            items: Vec::new(),
                            ascent: base_ascent,
                            descent: base_descent,
                        });
                        cur_x = 0.0;
                    }
                    if let Some(line) = lines.last_mut() {
                        line.ascent = line.ascent.max(baseline_from_top + 2.0);
                        line.descent = line.descent.max((size.y - baseline_from_top) + 2.0);
                        line.items.push(LineItem::Math {
                            tex,
                            size,
                            baseline_from_top,
                        });
                        cur_x += size.x;
                    }
                } else {
                    let galley = ui.fonts_mut(|f| {
                        f.layout_no_wrap(
                            format!("${latex}$"),
                            FontId::monospace(fsize * 0.9),
                            ctx.palette.accent,
                        )
                    });
                    let w = galley.rect.width();
                    if cur_x > 0.0 && cur_x + w > max_w {
                        lines.push(MeasuredLine {
                            items: Vec::new(),
                            ascent: base_ascent,
                            descent: base_descent,
                        });
                        cur_x = 0.0;
                    }
                    if let Some(line) = lines.last_mut() {
                        line.items.push(LineItem::Text {
                            galley,
                            link: None,
                            code_bg: true,
                        });
                        cur_x += w;
                    }
                }
            }
        }
    }

    let _ = ppp;
    let extra_gap = (fsize * (ctx.line_height - 1.35)).max(2.0);
    let total_h: f32 = lines
        .iter()
        .map(|l| l.ascent + l.descent + extra_gap)
        .sum::<f32>()
        .max(fsize);
    let (block_rect, _) = ui.allocate_exact_size(Vec2::new(max_w, total_h), Sense::hover());
    let mut y = block_rect.top();
    for line in lines {
        let baseline = y + line.ascent;
        let mut x = block_rect.left();
        for item in line.items {
            match item {
                LineItem::Text {
                    galley,
                    link,
                    code_bg,
                } => {
                    let gw = galley.rect.width();
                    let gh = galley.rect.height();
                    let pos = Pos2::new(x, baseline - base_ascent);
                    let r = Rect::from_min_size(pos, Vec2::new(gw, gh));
                    if code_bg {
                        ui.painter()
                            .rect_filled(r.expand(2.0), 3.0, ctx.palette.code_bg);
                    }
                    ui.painter().galley(pos, galley, ctx.palette.ink);
                    if let Some(target) = link {
                        let resp = ui.interact(
                            r,
                            ui.scope_id().with(("flow-link", r.min.x as i32, r.min.y as i32)),
                            Sense::click(),
                        );
                        if resp.hovered() {
                            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                        }
                        if resp.clicked() {
                            *ctx.clicked_link = Some(target);
                        }
                    }
                    x += gw;
                }
                LineItem::Ruby { base, rt } => {
                    let bw = base.rect.width();
                    let rw = rt.rect.width();
                    let atom_w = bw.max(rw) + 2.0;
                    let base_x = x + (atom_w - bw) * 0.5;
                    let rt_x = x + (atom_w - rw) * 0.5;
                    let base_y = baseline - base_ascent;
                    let rt_y = base_y - rt.rect.height() - 1.0;
                    ui.painter().galley(Pos2::new(rt_x, rt_y), rt, ctx.palette.muted);
                    ui.painter().galley(Pos2::new(base_x, base_y), base, ctx.palette.ink);
                    x += atom_w;
                }
                LineItem::Math {
                    tex,
                    size,
                    baseline_from_top,
                } => {
                    let pos = Pos2::new(x, baseline - baseline_from_top);
                    let r = Rect::from_min_size(pos, size);
                    ui.painter().image(
                        tex,
                        r,
                        Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                        Color32::WHITE,
                    );
                    x += size.x;
                }
            }
        }
        y += line.ascent + line.descent + extra_gap;
    }
}

fn draw_flow_block(ui: &mut Ui, block: &Block, ctx: &mut FlowDrawCtx<'_>) {
    match block {
        Block::Heading { level, inlines, .. } => {
            let mul = match level {
                1 => 1.65,
                2 => 1.38,
                3 => 1.2,
                _ => 1.08,
            };
            ui.add_space(6.0);
            draw_inline_flow(ui, inlines, mul, true, ctx);
        }
        Block::Paragraph(inlines) => {
            draw_inline_flow(ui, inlines, 1.0, false, ctx);
        }
        Block::Quote(inner) => {
            Frame::new()
                .inner_margin(Margin {
                    left: 14,
                    right: 0,
                    top: 4,
                    bottom: 4,
                })
                .stroke(Stroke::new(1.0, ctx.palette.line))
                .show(ui, |ui| {
                    for b in inner {
                        draw_flow_block(ui, b, ctx);
                    }
                });
        }
        Block::List { ordered, items } => {
            for (idx, item) in items.iter().enumerate() {
                ui.horizontal_top(|ui| {
                    let prefix = if *ordered {
                        format!("{}. ", idx + 1)
                    } else {
                        "• ".to_owned()
                    };
                    ui.label(RichText::new(prefix).color(ctx.palette.muted));
                    ui.vertical(|ui| {
                        for b in item {
                            draw_flow_block(ui, b, ctx);
                        }
                    });
                });
            }
        }
        Block::Code { code } => {
            Frame::new()
                .fill(ctx.palette.code_bg)
                .stroke(Stroke::new(1.0, ctx.palette.line))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(
                        RichText::new(code)
                            .monospace()
                            .size((ctx.font_size * 0.85).max(12.0))
                            .color(ctx.palette.ink),
                    );
                });
        }
        Block::Table { headers, rows } => {
            Frame::new()
                .stroke(Stroke::new(1.0, ctx.palette.line))
                .inner_margin(Margin::same(6))
                .show(ui, |ui| {
                    if !headers.is_empty() {
                        ui.horizontal_top(|ui| {
                            let col_w = (ctx.max_width / headers.len().max(1) as f32).max(60.0);
                            for cell in headers {
                                ui.allocate_ui(Vec2::new(col_w, 20.0), |ui| {
                                    draw_inline_flow(ui, cell, 0.95, true, ctx);
                                });
                            }
                        });
                        ui.separator();
                    }
                    for row in rows {
                        ui.horizontal_top(|ui| {
                            let col_w = (ctx.max_width / row.len().max(1) as f32).max(60.0);
                            for cell in row {
                                ui.allocate_ui(Vec2::new(col_w, 20.0), |ui| {
                                    draw_inline_flow(ui, cell, 0.95, false, ctx);
                                });
                            }
                        });
                    }
                });
        }
        Block::DisplayMath { latex } => {
            if let Some((tex, size, _)) = get_or_load_math(
                ui,
                latex,
                true,
                ctx.font_size * 1.15,
                ctx.palette.ink,
                ctx.math_cache,
                ctx.math_textures,
            ) {
                let scale = if size.x > ctx.max_width {
                    ctx.max_width / size.x
                } else {
                    1.0
                };
                let draw_size = size * scale;
                ui.add_space(6.0);
                let (slot, _) =
                    ui.allocate_exact_size(Vec2::new(ctx.max_width, draw_size.y), Sense::hover());
                let pos = Pos2::new(slot.center().x - draw_size.x * 0.5, slot.top());
                ui.painter().image(
                    tex,
                    Rect::from_min_size(pos, draw_size),
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                ui.add_space(6.0);
            } else {
                Frame::new()
                    .fill(ctx.palette.code_bg)
                    .corner_radius(CornerRadius::same(6))
                    .inner_margin(Margin::same(8))
                    .show(ui, |ui| {
                        ui.label(RichText::new(latex).monospace().color(ctx.palette.ink));
                    });
            }
        }
        Block::Image { path, alt } => {
            let key = (ctx.book_id.to_owned(), path.clone());
            if !ctx.image_textures.contains_key(&key) {
                let loaded = ctx.resources.get(path).and_then(|bytes| {
                    decode_resource_image(path, bytes).map(|(w, h, rgba)| {
                        let img =
                            ColorImage::from_rgba_unmultiplied([w as usize, h as usize], &rgba);
                        ui.ctx().load_texture(
                            format!("res-{}-{}", ctx.book_id, path),
                            img,
                            TextureOptions::LINEAR,
                        )
                    })
                });
                ctx.image_textures.insert(key.clone(), loaded);
            }
            if let Some(Some(tex)) = ctx.image_textures.get(&key) {
                let raw = tex.size_vec2();
                let scale = (ctx.max_width / raw.x.max(1.0)).min(1.0);
                let draw_size = raw * scale;
                let (slot, _) =
                    ui.allocate_exact_size(Vec2::new(ctx.max_width, draw_size.y), Sense::hover());
                let img_rect = Rect::from_min_size(
                    Pos2::new(slot.center().x - draw_size.x * 0.5, slot.top()),
                    draw_size,
                );
                ui.painter().image(
                    tex.id(),
                    img_rect,
                    Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                    Color32::WHITE,
                );
                let resp = ui.interact(
                    img_rect,
                    ui.scope_id().with(("flow-img", path)),
                    Sense::click(),
                );
                if resp.secondary_clicked()
                    && let Some(pointer) = ui.ctx().pointer_latest_pos()
                    && let Some(bytes) = ctx.resources.get(path)
                    && let Some((w, h, rgba)) = decode_resource_image(path, bytes)
                    && let Some(png) = encode_rgba_png(w, h, &rgba)
                {
                    let fname = Path::new(path)
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .map(|s| format!("{s}.png"))
                        .unwrap_or_else(|| "image.png".to_owned());
                    *ctx.requested_menu = Some(ImageContextMenu {
                        pos: pointer,
                        label: "复制图像".to_owned(),
                        filename: fname,
                        png_bytes: Some(png),
                        extra_text: None,
                    });
                }
            } else if !alt.is_empty() {
                ui.label(RichText::new(format!("[图片: {alt}]")).color(ctx.palette.muted));
            }
        }
        Block::Rule => {
            ui.separator();
        }
    }
}

fn decode_resource_image(path: &str, bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    if path.to_ascii_lowercase().ends_with(".svg") {
        let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
        let size = tree.size().to_int_size();
        let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())?;
        resvg::render(
            &tree,
            resvg::tiny_skia::Transform::identity(),
            &mut pixmap.as_mut(),
        );
        // Convert premultiplied RGBA -> straight RGBA
        let mut rgba = pixmap.take();
        unpremultiply_rgba(&mut rgba);
        return Some((size.width(), size.height(), rgba));
    }
    let dyn_img = image::load_from_memory(bytes).ok()?;
    let rgba = dyn_img.into_rgba8();
    let (w, h) = rgba.dimensions();
    Some((w, h, rgba.into_raw()))
}

fn encode_rgba_png(width: u32, height: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(width, height, rgba.to_vec())?;
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

fn crop_rgba_png(
    src_w: u32,
    src_h: u32,
    rgba: &[u8],
    x: u32,
    y: u32,
    w: u32,
    h: u32,
) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(src_w, src_h, rgba.to_vec())?;
    let cw = w.min(src_w.saturating_sub(x)).max(1);
    let ch = h.min(src_h.saturating_sub(y)).max(1);
    let cropped = image::imageops::crop_imm(&img, x, y, cw, ch).to_image();
    let mut out = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(cropped)
        .write_to(&mut out, image::ImageFormat::Png)
        .ok()?;
    Some(out.into_inner())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn extract_snippet(text: &str, match_byte: usize) -> String {
    let start = text[..match_byte]
        .char_indices()
        .rev()
        .nth(18)
        .map_or(0, |(i, _)| i);
    let end = text[match_byte..]
        .char_indices()
        .nth(48)
        .map_or(text.len(), |(i, _)| match_byte + i);
    text[start..end].replace('\n', " ")
}

fn install_system_fonts(ctx: &Context) {
    let mut defs = FontDefinitions::default();
    let mut search_dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("WINDIR") {
            search_dirs.push(PathBuf::from(root).join("Fonts"));
        } else {
            search_dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            search_dirs.push(PathBuf::from(root).join(r"Microsoft\Windows\Fonts"));
        }
    }
    let candidates = [
        ("cjk_deng", &["Deng.ttf", "deng.ttf"][..]),
        ("cjk_msyh", &["msyh.ttc"][..]),
        ("cjk_simsun", &["simsun.ttc"][..]),
        ("jp_yugoth", &["YuGothR.ttc", "meiryo.ttc", "msgothic.ttc"][..]),
    ];
    let mut loaded_keys = Vec::new();
    for (key, files) in candidates {
        for dir in &search_dirs {
            if let Some(bytes) = files.iter().find_map(|f| fs::read(dir.join(f)).ok()) {
                defs.font_data
                    .insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                loaded_keys.push(key.to_owned());
                break;
            }
        }
    }
    if !loaded_keys.is_empty() {
        for fam in [FontFamily::Proportional, FontFamily::Monospace] {
            if let Some(list) = defs.families.get_mut(&fam) {
                for key in &loaded_keys {
                    list.push(key.clone());
                }
            }
        }
    }
    ctx.set_fonts(defs);
}

pub fn render_app_icon() -> Option<egui::IconData> {
    let tree = resvg::usvg::Tree::from_data(
        include_bytes!("../assets/logo.svg"),
        &resvg::usvg::Options::default(),
    )
    .ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(64, 64)?;
    let sx = 64.0 / tree.size().width().max(1.0);
    let sy = 64.0 / tree.size().height().max(1.0);
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(sx, sy),
        &mut pixmap.as_mut(),
    );
    let mut rgba = pixmap.take();
    unpremultiply_rgba(&mut rgba);
    Some(egui::IconData {
        rgba,
        width: 64,
        height: 64,
    })
}

fn unpremultiply_rgba(rgba: &mut [u8]) {
    let (chunks, _) = rgba.as_chunks_mut::<4>();
    for px in chunks {
        let a = u16::from(px[3]);
        if a > 0 && a < 255 {
            px[0] = ((u16::from(px[0]) * 255) / a).min(255) as u8;
            px[1] = ((u16::from(px[1]) * 255) / a).min(255) as u8;
            px[2] = ((u16::from(px[2]) * 255) / a).min(255) as u8;
        }
    }
}
