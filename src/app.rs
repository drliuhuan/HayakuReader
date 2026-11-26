use crate::{
    flow::{Block, FlowDocument, Inline, LinkTarget, OutlineEntry},
    highlight::{self, CodeLang},
    math::MathCache,
    pdf::{PdfDoc, PdfLinkItem, PdfTextRun},
    platform::{self, DownloadProgress, InstallationStatus, Release},
    storage::{
        self, Anchor, Annotation, BookState, EXTENSIONS, Position, Recent, Settings, TabEntry,
        Workspace,
    },
    theme::{Palette, ThemeMode, smart_invert_rgba},
    word::{Word, WordTextRun},
};
use egui::{
    Align, CentralPanel, Color32, ColorImage, Context, CornerRadius, CursorIcon, FontData,
    FontDefinitions, FontFamily, FontId, Frame, Id, Key, Margin, Panel, PointerButton, Pos2, Rect,
    ResizeDirection, RichText, ScrollArea, Sense, Stroke, StrokeKind, TextureHandle,
    TextureOptions, Ui, UiBuilder, Vec2, ViewportCommand,
    text::{CCursor, LayoutJob},
};
use std::{
    collections::{HashMap, VecDeque},
    fs,
    io::Cursor,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TAB_SLEEP_IDLE_MS: u64 = 5 * 60 * 1000;
const TAB_SLEEP_MAX_AWAKE: usize = 5;
const SPLASH_DIRECT_DURATION: Duration = Duration::from_secs(1);
const INTRO_ZH_MD: &str = include_str!("../docs/intro.zh.md");

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

enum SplashState {
    Timed {
        started_at: Instant,
        duration: Duration,
        frames_rendered: u8,
        restored: bool,
    },
    LoadingFile {
        primary: PathBuf,
        extra_sleeping: Vec<PathBuf>,
        frames_rendered: u8,
    },
    Done,
}

enum LoadedSource {
    Pdf(PdfDoc),
    Word(Word),
    Flow(Arc<FlowDocument>),
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
    fragment: String,
    fraction: f32,
}

struct CachedPageTexture {
    scale_key: u32,
    smart_invert: bool,
    links: Vec<PdfLinkItem>,
    pdf_runs: Vec<PdfTextRun>,
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
    selected_text: Option<String>,
    extra_text: Option<String>,
}

struct LinkWarningModal {
    target: String,
    can_open: bool,
}

struct AnnotationEditorModal {
    existing_id: Option<String>,
    kind: String,
    color: String,
    text: String,
    parts: Vec<Anchor>,
    position: Option<Position>,
}

struct SearchMatch {
    unit: usize,
    snippet: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum SidebarTab {
    Recent,
    Contents,
    Notes,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct FlowTextPos {
    item_idx: usize,
    char_idx: usize,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
struct FixedTextPos {
    page: usize,
    run_idx: usize,
    char_idx: usize,
}

#[derive(Clone, PartialEq, Eq)]
struct FlowCacheKey {
    book_id: String,
    is_code_mode: bool,
    width_px: i32,
    font_size_tenths: u32,
    line_height_hundredths: u32,
    dark: bool,
}

struct HitLinkBox {
    rel_rect: Rect,
    target: LinkTarget,
}

enum FlowVisual {
    Separator,
    FallbackBlock,
    SingleJob {
        job: LayoutJob,
        pad_top: f32,
        link_spans: Vec<(usize, usize, LinkTarget)>,
        code_box: bool,
        quote_bar: bool,
    },
    CodeLine {
        line_no_text: String,
        code_job: LayoutJob,
        font_size: f32,
    },
    RichSpec {
        scale_mul: f32,
        pad_top: f32,
    },
}

enum FlowSourceRef {
    Separator,
    Block { chapter_idx: usize, block_idx: usize },
    CodeLine {
        line_no: usize,
        text: String,
        lang: CodeLang,
    },
}

struct FlowItemEntry {
    chapter_idx: usize,
    source: FlowSourceRef,
    height: f32,
    visual: Option<FlowVisual>,
    plain: String,
}

struct FlowLayoutCache {
    key: FlowCacheKey,
    items: Vec<FlowItemEntry>,
    chapter_first_item: Vec<usize>,
    anchor_to_item: HashMap<(usize, String), usize>,
}

enum BgEvent {
    ReleaseChecked(Result<Option<Release>, String>),
    UpdateFinished(Result<platform::Restart, String>),
    InboxFiles(Vec<PathBuf>),
    RequestRestart,
}

pub struct HayakuApp {
    data: PathBuf,
    settings: Settings,
    recent: Vec<Recent>,
    workspace: Workspace,
    tabs: Vec<TabEntry>,
    active_book: Option<OpenBook>,
    background_books: HashMap<String, OpenBook>,
    last_sleep_check: Instant,
    tab_sleep_hint_open: bool,
    tab_sleep_hint_tab_id: Option<String>,
    tab_screen_rects: HashMap<String, Rect>,
    tabs_scroll_offset: f32,
    tabs_scroll_max: f32,
    scroll_active_tab_into_view: bool,
    smart_invert_btn_rect: Option<Rect>,
    splash: SplashState,
    sidebar_open: bool,
    sidebar_pinned: bool,
    sidebar_tab: SidebarTab,
    active_outline_idx: Option<usize>,
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
    pending_scroll_y: Option<f32>,
    last_scroll_y: f32,
    last_viewport_h: f32,
    last_content_h: f32,

    flow_cache: Option<FlowLayoutCache>,
    flow_sel_anchor: Option<FlowTextPos>,
    flow_sel_head: Option<FlowTextPos>,
    flow_sel_dragging: bool,
    flow_width_dragging: bool,

    fixed_sel_anchor: Option<FixedTextPos>,
    fixed_sel_head: Option<FixedTextPos>,
    fixed_sel_dragging: bool,
    fixed_all_selected: bool,
    last_selection_rect: Option<Rect>,

    math_cache: MathCache,
    math_textures: HashMap<MathTexKey, Option<CachedMathTex>>,
    image_textures: HashMap<(String, String), Option<TextureHandle>>,
    logo_texture: Option<TextureHandle>,

    status_msg: Option<(String, Option<Instant>)>,
    context_menu: Option<ImageContextMenu>,
    link_warning: Option<LinkWarningModal>,
    annotation_editor: Option<AnnotationEditorModal>,
    smart_invert_hint_open: bool,

    install_status: Option<InstallationStatus>,
    show_install_dialog: bool,
    install_selected: HashMap<String, bool>,

    pending_release: Option<Release>,
    show_update_dialog: bool,
    update_progress: Arc<Mutex<Option<DownloadProgress>>>,
    update_downloading: bool,

    instance_lock: Option<fs::File>,
    bg_rx: mpsc::Receiver<BgEvent>,
    bg_tx: mpsc::Sender<BgEvent>,
    bg_stop: Arc<AtomicBool>,
    scroll_fraction: f32,
    current_unit: usize,
}

impl HayakuApp {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        data: PathBuf,
        initial: Option<PathBuf>,
        mut dropped: VecDeque<PathBuf>,
        instance_lock: Option<fs::File>,
    ) -> Self {
        install_system_fonts(&cc.egui_ctx);
        let logo_texture = render_app_icon().map(|icon| {
            let img = ColorImage::from_rgba_unmultiplied(
                [icon.width as usize, icon.height as usize],
                &icon.rgba,
            );
            cc.egui_ctx
                .load_texture("app_logo", img, TextureOptions::LINEAR)
        });
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
        let bg_stop = Arc::new(AtomicBool::new(false));
        let splash = if let Some(first_path) = initial {
            let mut all_paths = vec![first_path];
            while let Some(extra) = dropped.pop_front() {
                all_paths.push(extra);
            }
            let primary = all_paths.pop().expect("non-empty startup paths");
            SplashState::LoadingFile {
                primary,
                extra_sleeping: all_paths,
                frames_rendered: 0,
            }
        } else {
            SplashState::Timed {
                started_at: Instant::now(),
                duration: SPLASH_DIRECT_DURATION,
                frames_rendered: 0,
                restored: false,
            }
        };

        let app = Self {
            data,
            settings,
            recent,
            workspace,
            tabs,
            active_book: None,
            background_books: HashMap::new(),
            last_sleep_check: Instant::now(),
            tab_sleep_hint_open: false,
            tab_sleep_hint_tab_id: None,
            tab_screen_rects: HashMap::new(),
            tabs_scroll_offset: 0.0,
            tabs_scroll_max: 0.0,
            scroll_active_tab_into_view: false,
            smart_invert_btn_rect: None,
            splash,
            sidebar_open: false,
            sidebar_pinned: false,
            sidebar_tab: SidebarTab::Contents,
            active_outline_idx: None,
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
            pending_scroll_y: None,
            last_scroll_y: 0.0,
            last_viewport_h: 600.0,
            last_content_h: 600.0,
            flow_cache: None,
            flow_sel_anchor: None,
            flow_sel_head: None,
            flow_sel_dragging: false,
            flow_width_dragging: false,
            fixed_sel_anchor: None,
            fixed_sel_head: None,
            fixed_sel_dragging: false,
            fixed_all_selected: false,
            last_selection_rect: None,
            math_cache: MathCache::default(),
            math_textures: HashMap::new(),
            image_textures: HashMap::new(),
            logo_texture,
            status_msg: None,
            context_menu: None,
            link_warning: None,
            annotation_editor: None,
            smart_invert_hint_open: false,
            install_status,
            show_install_dialog: false,
            install_selected,
            pending_release: None,
            show_update_dialog: false,
            update_progress: Arc::new(Mutex::new(None)),
            update_downloading: false,
            instance_lock,
            bg_rx,
            bg_tx,
            bg_stop,
            scroll_fraction: 0.0,
            current_unit: 0,
        };

        let watch_ctx = cc.egui_ctx.clone();
        let watch_data = app.data.clone();
        let watch_tx = app.bg_tx.clone();
        let watch_stop = Arc::clone(&app.bg_stop);
        std::thread::spawn(move || {
            while !watch_stop.load(Ordering::Relaxed) {
                if watch_data.join("request-restart").is_file() {
                    let _ = fs::remove_file(watch_data.join("request-restart"));
                    let _ = watch_tx.send(BgEvent::RequestRestart);
                    watch_ctx.request_repaint();
                }
                if let Some(paths) = drain_inbox_dir(&watch_data.join("inbox")) {
                    let _ = watch_tx.send(BgEvent::InboxFiles(paths));
                    watch_ctx.request_repaint();
                }
                std::thread::sleep(Duration::from_millis(150));
            }
        });

        app.spawn_release_check(&cc.egui_ctx, false);
        app
    }

    fn release_instance_lock(&mut self) {
        if let Some(lock) = self.instance_lock.take() {
            #[cfg(windows)]
            let _ = lock.unlock();
            drop(lock);
        }
    }

    fn active_book_has_outline_or_notes(&self) -> bool {
        let Some(book) = &self.active_book else {
            return false;
        };
        if !book.state.annotations.is_empty() {
            return true;
        }
        match &book.source {
            LoadedSource::Pdf(p) => !p.outline().is_empty(),
            LoadedSource::Word(w) => !w.metadata.outline.is_empty(),
            LoadedSource::Flow(f) => !f.outline.is_empty(),
        }
    }

    fn sync_auto_sidebar(&mut self, select_contents_if_opened: bool) {
        if self.sidebar_pinned {
            self.sidebar_open = true;
        } else if self.active_book_has_outline_or_notes() {
            self.sidebar_open = true;
            if select_contents_if_opened {
                self.sidebar_tab = SidebarTab::Contents;
            }
        } else {
            self.sidebar_open = false;
        }
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

    fn active_book_uses_smart_invert(&self) -> bool {
        self.active_book
            .as_ref()
            .is_some_and(|b| matches!(b.source, LoadedSource::Pdf(_) | LoadedSource::Word(_)))
    }

    fn is_tab_sleeping(&self, id: &str) -> bool {
        self.active_book.as_ref().is_none_or(|b| b.id != id)
            && !self.background_books.contains_key(id)
    }

    fn is_modal_open(&self) -> bool {
        !matches!(self.splash, SplashState::Done)
            || self.link_warning.is_some()
            || self.annotation_editor.is_some()
            || self.smart_invert_hint_open
            || self.tab_sleep_hint_open
            || self.show_install_dialog
            || self.show_update_dialog
    }

    fn copy_text_reliable(&mut self, ctx: &Context, text: String, success_msg: &str) {
        if text.is_empty() {
            return;
        }
        ctx.copy_text(text.clone());
        #[cfg(windows)]
        {
            if let Err(e) = platform::set_clipboard_text(&text) {
                self.set_status(format!("复制失败：{e}"), Some(3));
                return;
            }
        }
        self.set_status(success_msg, Some(2));
    }

    fn on_splash_finished(&mut self) {
        if let Some(st) = &self.install_status
            && !st.installation_exists
            && !self.settings.install_dismissed
        {
            self.show_install_dialog = true;
        }
        self.maybe_trigger_smart_invert_hint();
    }

    fn restore_active_session_tab_only(&mut self) {
        if !self.settings.restore_session || self.tabs.is_empty() {
            return;
        }
        let target_id = self
            .workspace
            .active
            .clone()
            .filter(|id| self.tabs.iter().any(|t| t.id == *id))
            .or_else(|| self.tabs.last().map(|t| t.id.clone()));
        if let Some(active_id) = target_id
            && let Some(rec) = self.recent.iter().find(|r| r.id == active_id).cloned()
            && rec.path.is_file()
        {
            self.open_path(&rec.path);
        }
    }

    fn register_sleeping_path(&mut self, path: &Path) {
        let Ok(normalized) = storage::dunce_path(path) else {
            return;
        };
        if !normalized.is_file() {
            return;
        }
        let title = normalized
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "文档".to_owned());
        let id = if let Some(rec) = self.recent.iter().find(|r| r.path == normalized) {
            rec.id.clone()
        } else {
            let new_id = uuid::Uuid::new_v4().to_string();
            self.recent.push(Recent {
                id: new_id.clone(),
                path: normalized,
                digest: String::new(),
            });
            self.recent.truncate(200);
            let _ = storage::write_json(&self.data.join("recent.json"), &self.recent);
            new_id
        };
        if !self.tabs.iter().any(|t| t.id == id) {
            self.tabs.push(TabEntry {
                id,
                title,
                last_used: 0,
            });
        }
        self.save_workspace();
    }

    fn touch_tab(&mut self, id: &str, now_ms: u64) {
        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.last_used = now_ms;
        }
    }

    fn park_active_book(&mut self, now_ms: u64) {
        self.save_active_book();
        if let Some(book) = self.active_book.take() {
            let id = book.id.clone();
            self.touch_tab(&id, now_ms);
            self.background_books.insert(id, book);
        }
    }

    fn activate_loaded_book(&mut self, book: OpenBook, now_ms: u64, show_opened_status: bool) {
        let id = book.id.clone();
        let title = book.title.clone();
        let pos = book.state.position.clone().unwrap_or_default();
        self.current_unit = pos.index;
        self.scroll_fraction = pos.fraction;
        self.pending_jump = Some(PendingJump {
            chapter: pos.index,
            fragment: String::new(),
            fraction: pos.fraction,
        });

        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
            tab.title = title;
            tab.last_used = now_ms;
        } else {
            self.tabs.push(TabEntry {
                id: id.clone(),
                title,
                last_used: now_ms,
            });
        }

        self.active_book = Some(book);
        self.sync_auto_sidebar(true);
        self.fixed_zoom = None;
        self.flow_cache = None;
        self.active_outline_idx = None;
        self.flow_sel_anchor = None;
        self.flow_sel_head = None;
        self.flow_sel_dragging = false;
        self.fixed_sel_anchor = None;
        self.fixed_sel_head = None;
        self.fixed_sel_dragging = false;
        self.fixed_all_selected = false;
        self.search_matches.clear();
        self.search_cursor = 0;
        self.scroll_active_tab_into_view = true;
        if !self.active_book_uses_smart_invert() {
            self.smart_invert_hint_open = false;
        }
        self.sleep_inactive_tabs_at(now_ms);
        self.save_workspace();
        if show_opened_status {
            self.set_status("打开成功", Some(2));
        }
        self.maybe_trigger_smart_invert_hint();
    }

    fn sleep_inactive_tabs_at(&mut self, now_ms: u64) -> Vec<String> {
        let active_id = self.active_book.as_ref().map(|b| b.id.clone());
        let mut ordered: Vec<(String, u64, bool)> = self
            .tabs
            .iter()
            .map(|t| (t.id.clone(), t.last_used, active_id.as_deref() == Some(t.id.as_str())))
            .collect();
        ordered.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| b.1.cmp(&a.1)));

        let mut slept = Vec::new();
        for (rank, (id, last_used, is_active)) in ordered.into_iter().enumerate() {
            if is_active || !self.background_books.contains_key(&id) {
                continue;
            }
            let outside_recent = rank >= TAB_SLEEP_MAX_AWAKE;
            let idle_expired = last_used > 0 && now_ms.saturating_sub(last_used) >= TAB_SLEEP_IDLE_MS;
            if outside_recent || idle_expired {
                self.background_books.remove(&id);
                self.page_cache.retain(|(bid, _), _| bid != &id);
                self.image_textures.retain(|(bid, _), _| bid != &id);
                slept.push(id);
            }
        }
        if !slept.is_empty() && !self.settings.tab_sleep_hint_seen {
            self.tab_sleep_hint_tab_id = Some(slept[0].clone());
            if matches!(self.splash, SplashState::Done)
                && !self.show_install_dialog
                && !self.show_update_dialog
                && !self.smart_invert_hint_open
            {
                self.tab_sleep_hint_open = true;
            }
        }
        slept
    }

    fn switch_to_tab(&mut self, id: &str) {
        let now_ms = now_epoch_ms();
        if self.active_book.as_ref().is_some_and(|b| b.id == id) {
            self.touch_tab(id, now_ms);
            self.scroll_active_tab_into_view = true;
            return;
        }
        if let Some(book) = self.background_books.remove(id) {
            self.context_menu = None;
            self.park_active_book(now_ms);
            self.activate_loaded_book(book, now_ms, false);
            return;
        }
        if let Some(rec) = self.recent.iter().find(|r| r.id == id).cloned() {
            self.open_path(&rec.path);
        } else {
            self.set_status("找不到该标签页对应的文件路径。", Some(3));
        }
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

    fn spawn_release_check(&self, ctx: &Context, manual: bool) {
        let tx = self.bg_tx.clone();
        let repaint_ctx = ctx.clone();
        std::thread::spawn(move || {
            let res = platform::check_latest_release();
            if manual || matches!(&res, Ok(Some(_))) {
                let _ = tx.send(BgEvent::ReleaseChecked(res));
                repaint_ctx.request_repaint();
            }
        });
    }

    fn maybe_trigger_smart_invert_hint(&mut self) {
        if matches!(self.splash, SplashState::Done)
            && self.active_book_uses_smart_invert()
            && self.is_smart_invert()
            && !self.settings.smart_invert_hint_seen
            && !self.show_install_dialog
            && !self.show_update_dialog
        {
            self.show_settings = false;
            self.smart_invert_hint_open = true;
        }
    }

    fn open_path(&mut self, path: &Path) {
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
        let existing_by_path = self
            .recent
            .iter()
            .find(|r| r.path == normalized)
            .map(|r| r.id.clone());
        if let Some(id) = existing_by_path.as_deref() {
            if self.active_book.as_ref().is_some_and(|b| b.id == id) {
                let now_ms = now_epoch_ms();
                self.touch_tab(id, now_ms);
                self.scroll_active_tab_into_view = true;
                return;
            }
            if let Some(book) = self.background_books.remove(id) {
                let now_ms = now_epoch_ms();
                self.park_active_book(now_ms);
                self.activate_loaded_book(book, now_ms, false);
                return;
            }
        }

        let now_ms = now_epoch_ms();
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
        let existing_id = existing_by_path.or_else(|| {
            self.recent
                .iter()
                .find(|r| !r.digest.is_empty() && r.digest == digest && !r.path.exists())
                .map(|r| r.id.clone())
        });
        let id = existing_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut state: BookState =
            storage::read_json(&self.data.join(format!("{id}.json"))).unwrap_or_default();

        let source_res = match ext.as_str() {
            "pdf" => PdfDoc::open(&normalized).map(LoadedSource::Pdf),
            "doc" | "docx" => Word::open(&normalized).map(LoadedSource::Word),
            "txt" => crate::flow::open_text(&normalized, state.encoding.as_deref())
                .map(|f| LoadedSource::Flow(Arc::new(f))),
            "md" | "markdown" => {
                crate::flow::open_markdown(&normalized).map(|f| LoadedSource::Flow(Arc::new(f)))
            }
            "html" | "htm" | "xhtml" => {
                crate::flow::open_html(&normalized).map(|f| LoadedSource::Flow(Arc::new(f)))
            }
            "xml" => crate::flow::open_xml(&normalized).map(|f| LoadedSource::Flow(Arc::new(f))),
            "epub" => crate::flow::open_epub(&normalized).map(|f| LoadedSource::Flow(Arc::new(f))),
            "azw" | "azw3" | "mobi" => {
                crate::flow::open_kindle(&normalized).map(|f| LoadedSource::Flow(Arc::new(f)))
            }
            _ => Err(
                "不支持此文件格式（支持 PDF/Word/EPUB/Kindle/HTML/XML/Markdown/TXT）".to_owned(),
            ),
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
        if kind == "xml" && state.view_mode.is_none() {
            state.view_mode = Some("code".to_owned());
        }

        self.park_active_book(now_ms);
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

        let book = OpenBook {
            id,
            title,
            digest,
            kind,
            source,
            state,
        };
        self.activate_loaded_book(book, now_ms, true);
    }

    fn close_tab(&mut self, id: &str) {
        if self.active_book.as_ref().is_some_and(|b| b.id == id) {
            self.save_active_book();
            self.active_book = None;
            self.flow_cache = None;
            self.smart_invert_hint_open = false;
        }
        self.background_books.remove(id);
        self.page_cache.retain(|(bid, _), _| bid != id);
        self.image_textures.retain(|(bid, _), _| bid != id);
        self.tab_screen_rects.remove(id);
        if self.tab_sleep_hint_tab_id.as_deref() == Some(id) {
            self.tab_sleep_hint_tab_id = None;
            self.tab_sleep_hint_open = false;
        }
        self.tabs.retain(|t| t.id != id);
        if self.active_book.is_none()
            && let Some(next_id) = self.tabs.last().map(|t| t.id.clone())
        {
            self.switch_to_tab(&next_id);
        } else {
            self.sync_auto_sidebar(false);
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
            LinkTarget::Internal { chapter, fragment } => {
                self.pending_jump = Some(PendingJump {
                    chapter: *chapter,
                    fragment: fragment.clone(),
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

    fn selected_flow_text(&self) -> String {
        let Some(cache) = &self.flow_cache else {
            return String::new();
        };
        let (Some(a), Some(b)) = (self.flow_sel_anchor, self.flow_sel_head) else {
            return String::new();
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            return String::new();
        }
        let mut out = Vec::new();
        let end_idx = end.item_idx.min(cache.items.len().saturating_sub(1));
        for (idx, item) in cache.items[..=end_idx].iter().enumerate().skip(start.item_idx) {
            let text = &item.plain;
            if text.is_empty() {
                continue;
            }
            let total_chars = text.chars().count();
            let c0 = if idx == start.item_idx {
                start.char_idx.min(total_chars)
            } else {
                0
            };
            let c1 = if idx == end.item_idx {
                end.char_idx.min(total_chars)
            } else {
                total_chars
            };
            if c1 > c0 {
                out.push(slice_chars(text, c0, c1));
            }
        }
        out.join("\n")
    }

    fn selected_fixed_text(&self) -> String {
        let Some(book) = &self.active_book else {
            return String::new();
        };
        if self.fixed_all_selected {
            return match &book.source {
                LoadedSource::Pdf(pdf) => (0..pdf.page_count())
                    .map(|p| pdf.page_text(p))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                LoadedSource::Word(word) => (0..word.metadata.pages.len())
                    .filter_map(|p| word.text_runs(p).ok())
                    .map(|runs| word_runs_plain(&runs))
                    .collect::<Vec<_>>()
                    .join("\n\n"),
                LoadedSource::Flow(_) => String::new(),
            };
        }
        let (Some(a), Some(b)) = (self.fixed_sel_anchor, self.fixed_sel_head) else {
            return String::new();
        };
        let (start, end) = if a <= b { (a, b) } else { (b, a) };
        if start == end {
            return String::new();
        }
        let mut parts = Vec::new();
        for page in start.page..=end.page {
            let key = (book.id.clone(), page);
            let Some(cached) = self.page_cache.get(&key) else {
                continue;
            };
            let run_count = cached.pdf_runs.len().max(cached.word_runs.len());
            if run_count == 0 {
                continue;
            }
            let r_start = if page == start.page { start.run_idx } else { 0 };
            let r_end = if page == end.page {
                end.run_idx.min(run_count.saturating_sub(1))
            } else {
                run_count.saturating_sub(1)
            };
            for ri in r_start..=r_end {
                let text = if !cached.pdf_runs.is_empty() {
                    &cached.pdf_runs[ri].text
                } else {
                    &cached.word_runs[ri].text
                };
                let len = text.chars().count();
                let c0 = if page == start.page && ri == start.run_idx {
                    start.char_idx.min(len)
                } else {
                    0
                };
                let c1 = if page == end.page && ri == end.run_idx {
                    end.char_idx.min(len)
                } else {
                    len
                };
                if c1 > c0 {
                    parts.push(slice_chars(text, c0, c1));
                }
            }
        }
        parts.join("")
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
                        let txt = word_runs_plain(&runs);
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
                fragment: String::new(),
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
                    self.release_instance_lock();
                    if platform::restart(restart_req).is_ok() {
                        ctx.send_viewport_cmd(ViewportCommand::Close);
                    }
                }
                BgEvent::UpdateFinished(Err(e)) => {
                    self.update_downloading = false;
                    self.set_status(format!("更新失败：{e}"), None);
                }
                BgEvent::InboxFiles(mut paths) => {
                    if let Some(primary) = paths.pop() {
                        for extra in paths {
                            self.register_sleeping_path(&extra);
                        }
                        self.open_path(&primary);
                        if !matches!(self.splash, SplashState::Done) {
                            self.splash = SplashState::Done;
                            self.on_splash_finished();
                        }
                    }
                    ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(ViewportCommand::Focus);
                    #[cfg(windows)]
                    platform::bring_current_process_window_to_front();
                }
                BgEvent::RequestRestart => {
                    self.save_active_book();
                    self.save_workspace();
                    ctx.send_viewport_cmd(ViewportCommand::Close);
                    return;
                }
            }
        }
    }

    fn apply_egui_visuals(&self, ctx: &Context) {
        for theme in [egui::Theme::Light, egui::Theme::Dark] {
            ctx.style_mut_of(theme, |s| s.interaction.selectable_labels = false);
        }
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
        visuals.slider_trailing_fill = true;
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, p.line);
        visuals.widgets.inactive.bg_fill = p.line;
        visuals.widgets.inactive.weak_bg_fill = p.surface;
        visuals.widgets.inactive.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.inactive.bg_stroke = Stroke::new(1.0, p.line);
        visuals.widgets.hovered.bg_fill = p.side;
        visuals.widgets.hovered.fg_stroke = Stroke::new(1.0, p.ink);
        visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, p.accent);
        ctx.set_visuals(visuals);
    }

    fn handle_shortcuts(&mut self, ctx: &Context) {
        if self.is_modal_open() {
            return;
        }
        let typing = ctx.egui_wants_keyboard_input();
        let (
            open,
            find,
            side,
            close_tab,
            next_tab,
            zoom_in,
            zoom_out,
            zoom_reset,
            f11,
            esc,
            wheel,
            copy,
            select_all,
            home,
            end,
            pg_up,
            pg_dn,
            space,
            shift,
            up,
            down,
        ) = ctx.input(|i| {
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
                i.events.iter().any(|e| matches!(e, egui::Event::Copy))
                    || (ctrl && i.key_pressed(Key::C)),
                ctrl && i.key_pressed(Key::A),
                !typing && !ctrl && i.key_pressed(Key::Home),
                !typing && !ctrl && i.key_pressed(Key::End),
                !typing && !ctrl && i.key_pressed(Key::PageUp),
                !typing && !ctrl && i.key_pressed(Key::PageDown),
                !typing && !ctrl && i.key_pressed(Key::Space),
                i.modifiers.shift,
                !typing && !ctrl && i.key_pressed(Key::ArrowUp),
                !typing && !ctrl && i.key_pressed(Key::ArrowDown),
            )
        });

        if open {
            self.pick_and_open_files();
        }
        if find && self.active_book.is_some() {
            self.show_search = !self.show_search;
        }
        if side {
            self.sidebar_pinned = !self.sidebar_pinned;
            self.sync_auto_sidebar(false);
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
            self.switch_to_tab(&next_id);
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

        if copy && !typing && self.active_book.is_some() {
            let is_flow = self
                .active_book
                .as_ref()
                .is_some_and(|b| matches!(b.source, LoadedSource::Flow(_)));
            let txt = if is_flow {
                self.selected_flow_text()
            } else {
                self.selected_fixed_text()
            };
            if !txt.is_empty() {
                self.copy_text_reliable(ctx, txt, "已复制选中文字。");
            }
        }

        if select_all && !typing && let Some(book) = &self.active_book {
            match &book.source {
                LoadedSource::Flow(_) => {
                    if let Some(cache) = &self.flow_cache
                        && !cache.items.is_empty()
                    {
                        let last = cache.items.len() - 1;
                        let last_chars = cache.items[last].plain.chars().count();
                        self.flow_sel_anchor = Some(FlowTextPos {
                            item_idx: 0,
                            char_idx: 0,
                        });
                        self.flow_sel_head = Some(FlowTextPos {
                            item_idx: last,
                            char_idx: last_chars,
                        });
                        self.set_status("已全选正文（Ctrl+C 可复制）。", Some(2));
                    }
                }
                LoadedSource::Pdf(_) | LoadedSource::Word(_) => {
                    self.fixed_all_selected = true;
                    self.fixed_sel_anchor = None;
                    self.fixed_sel_head = None;
                    self.set_status("已全选全文内容（Ctrl+C 可复制）。", Some(2));
                }
            }
        }

        if self.active_book.is_some() && !typing {
            let max_y = (self.last_content_h - self.last_viewport_h).max(0.0);
            let step_page = (self.last_viewport_h * 0.88).max(120.0);
            if home {
                self.pending_scroll_y = Some(0.0);
            } else if end {
                self.pending_scroll_y = Some(max_y);
            } else if pg_dn || (space && !shift) {
                self.pending_scroll_y = Some((self.last_scroll_y + step_page).min(max_y));
            } else if pg_up || (space && shift) {
                self.pending_scroll_y = Some((self.last_scroll_y - step_page).max(0.0));
            } else if down {
                self.pending_scroll_y = Some((self.last_scroll_y + 72.0).min(max_y));
            } else if up {
                self.pending_scroll_y = Some((self.last_scroll_y - 72.0).max(0.0));
            }
        }

        if zoom_in || zoom_out || zoom_reset || wheel != 0.0 {
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
                self.flow_cache = None;
                self.save_settings();
            }
        }
    }

    fn draw_resize_edges(&self, ctx: &Context) {
        if self.is_modal_open() || ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let rect = ctx.input(|i| i.viewport_rect());
        let (pos, pressed) = ctx.input(|i| (i.pointer.latest_pos(), i.pointer.primary_pressed()));
        let Some(pos) = pos else {
            return;
        };
        let edge = 5.0;
        let corner = 10.0;
        let left = pos.x <= rect.min.x + edge;
        let right = pos.x >= rect.max.x - edge;
        let top = pos.y <= rect.min.y + edge;
        let bottom = pos.y >= rect.max.y - edge;
        let c_left = pos.x <= rect.min.x + corner;
        let c_right = pos.x >= rect.max.x - corner;
        let c_top = pos.y <= rect.min.y + corner;
        let c_bottom = pos.y >= rect.max.y - corner;

        let hit = if c_top && c_left && (top || left) {
            Some((ResizeDirection::NorthWest, CursorIcon::ResizeNwSe))
        } else if c_top && c_right && (top || right) {
            Some((ResizeDirection::NorthEast, CursorIcon::ResizeNeSw))
        } else if c_bottom && c_left && (bottom || left) {
            Some((ResizeDirection::SouthWest, CursorIcon::ResizeNeSw))
        } else if c_bottom && c_right && (bottom || right) {
            Some((ResizeDirection::SouthEast, CursorIcon::ResizeNwSe))
        } else if top {
            Some((ResizeDirection::North, CursorIcon::ResizeVertical))
        } else if bottom {
            Some((ResizeDirection::South, CursorIcon::ResizeVertical))
        } else if left {
            Some((ResizeDirection::West, CursorIcon::ResizeHorizontal))
        } else if right {
            Some((ResizeDirection::East, CursorIcon::ResizeHorizontal))
        } else {
            None
        };
        if let Some((dir, cursor)) = hit {
            ctx.set_cursor_icon(cursor);
            if pressed {
                ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            }
        }
    }

    fn draw_top_bar(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let maximized = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        Panel::top("top_toolbar")
            .exact_size(46.0)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin {
                        left: 10,
                        right: 0,
                        top: 0,
                        bottom: 0,
                    }),
            )
            .show(ui, |ui| {
                ui.horizontal_centered(|ui| {
                    if let Some(logo) = &self.logo_texture {
                        ui.add(egui::Image::new((logo.id(), Vec2::splat(22.0))));
                    }
                    let (side_rect, side_resp) =
                        ui.allocate_exact_size(Vec2::new(30.0, 28.0), Sense::click());
                    let side_tip = if self.sidebar_pinned {
                        "取消始终显示侧边栏（切回自动模式）(Ctrl+B)"
                    } else {
                        "始终显示侧边栏（当前：自动模式）(Ctrl+B)"
                    };
                    let side_resp = side_resp.on_hover_text(side_tip);
                    if self.sidebar_pinned {
                        ui.painter().rect_filled(
                            side_rect,
                            5.0,
                            Color32::from_rgba_unmultiplied(
                                p.accent.r(),
                                p.accent.g(),
                                p.accent.b(),
                                36,
                            ),
                        );
                        ui.painter().rect_stroke(
                            side_rect,
                            5.0,
                            Stroke::new(1.0, p.accent),
                            StrokeKind::Inside,
                        );
                    } else if side_resp.hovered() {
                        ui.painter().rect_filled(side_rect, 5.0, p.side);
                    }
                    let sc = side_rect.center();
                    let icon_color = if self.sidebar_pinned { p.accent } else { p.ink };
                    for dy in [-4.5f32, 0.0, 4.5] {
                        ui.painter().line_segment(
                            [Pos2::new(sc.x - 6.5, sc.y + dy), Pos2::new(sc.x + 6.5, sc.y + dy)],
                            Stroke::new(1.6, icon_color),
                        );
                    }
                    if side_resp.clicked() {
                        self.sidebar_pinned = !self.sidebar_pinned;
                        self.sync_auto_sidebar(false);
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
                    let controls_reserve = 360.0;
                    let drag_width = (ui.available_width() - controls_reserve).max(40.0);
                    let (drag_rect, drag_resp) = ui.allocate_exact_size(
                        Vec2::new(drag_width, 36.0),
                        Sense::click_and_drag(),
                    );
                    ui.painter().text(
                        drag_rect.left_center() + Vec2::new(6.0, 0.0),
                        egui::Align2::LEFT_CENTER,
                        truncate_str(title, 48),
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
                        // Close button
                        let (close_rect, close_resp) =
                            ui.allocate_exact_size(Vec2::new(44.0, 44.0), Sense::click());
                        let close_resp = close_resp.on_hover_text("关闭");
                        if close_resp.hovered() {
                            ui.painter().rect_filled(
                                close_rect,
                                0.0,
                                Color32::from_rgb(0xc4, 0x2b, 0x1c),
                            );
                        }
                        let x_col = if close_resp.hovered() { Color32::WHITE } else { p.ink };
                        let cc = close_rect.center();
                        ui.painter().line_segment(
                            [cc - Vec2::splat(5.0), cc + Vec2::splat(5.0)],
                            Stroke::new(1.5, x_col),
                        );
                        ui.painter().line_segment(
                            [cc + Vec2::new(-5.0, 5.0), cc + Vec2::new(5.0, -5.0)],
                            Stroke::new(1.5, x_col),
                        );
                        if close_resp.clicked() {
                            self.save_active_book();
                            self.save_workspace();
                            ui.ctx().send_viewport_cmd(ViewportCommand::Close);
                        }

                        // Maximize / Restore button
                        let (max_rect, max_resp) =
                            ui.allocate_exact_size(Vec2::new(44.0, 44.0), Sense::click());
                        let max_resp =
                            max_resp.on_hover_text(if maximized { "还原" } else { "最大化" });
                        if max_resp.hovered() {
                            ui.painter().rect_filled(max_rect, 0.0, p.side);
                        }
                        let mc = max_rect.center();
                        if maximized {
                            ui.painter().rect_stroke(
                                Rect::from_center_size(mc + Vec2::new(1.5, -1.5), Vec2::splat(8.0)),
                                0.0,
                                Stroke::new(1.3, p.ink),
                                StrokeKind::Inside,
                            );
                            ui.painter().rect_filled(
                                Rect::from_center_size(mc + Vec2::new(-1.5, 1.5), Vec2::splat(8.0)),
                                0.0,
                                p.surface,
                            );
                            ui.painter().rect_stroke(
                                Rect::from_center_size(mc + Vec2::new(-1.5, 1.5), Vec2::splat(8.0)),
                                0.0,
                                Stroke::new(1.3, p.ink),
                                StrokeKind::Inside,
                            );
                        } else {
                            ui.painter().rect_stroke(
                                Rect::from_center_size(mc, Vec2::splat(10.0)),
                                0.0,
                                Stroke::new(1.3, p.ink),
                                StrokeKind::Inside,
                            );
                        }
                        if max_resp.clicked() {
                            ui.ctx()
                                .send_viewport_cmd(ViewportCommand::Maximized(!maximized));
                        }

                        // Minimize button
                        let (min_rect, min_resp) =
                            ui.allocate_exact_size(Vec2::new(44.0, 44.0), Sense::click());
                        let min_resp = min_resp.on_hover_text("最小化");
                        if min_resp.hovered() {
                            ui.painter().rect_filled(min_rect, 0.0, p.side);
                        }
                        let mnc = min_rect.center();
                        ui.painter().line_segment(
                            [mnc - Vec2::new(5.5, 0.0), mnc + Vec2::new(5.5, 0.0)],
                            Stroke::new(1.4, p.ink),
                        );
                        if min_resp.clicked() {
                            ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true));
                        }

                        ui.separator();
                        if ui.button("设置").clicked() {
                            self.show_settings = !self.show_settings;
                        }
                        if self.active_book.is_some() {
                            if ui.button("笔记").clicked() {
                                self.open_note_editor_from_selection();
                            }
                            if ui.button("书签").clicked() {
                                self.add_bookmark();
                            }
                            if ui.button("搜索").clicked() {
                                self.show_search = !self.show_search;
                            }
                            if self
                                .active_book
                                .as_ref()
                                .is_some_and(|b| matches!(b.kind, "markdown" | "html" | "xml"))
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
                                    self.flow_cache = None;
                                    self.flow_sel_anchor = None;
                                    self.flow_sel_head = None;
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

    fn clear_selection(&mut self) {
        self.flow_sel_anchor = None;
        self.flow_sel_head = None;
        self.flow_sel_dragging = false;
        self.fixed_sel_anchor = None;
        self.fixed_sel_head = None;
        self.fixed_sel_dragging = false;
        self.fixed_all_selected = false;
        self.last_selection_rect = None;
    }

    fn selected_anchors(&self) -> Vec<Anchor> {
        let Some(book) = &self.active_book else {
            return Vec::new();
        };
        match &book.source {
            LoadedSource::Pdf(_) | LoadedSource::Word(_) => {
                let (Some(a), Some(b)) = (self.fixed_sel_anchor, self.fixed_sel_head) else {
                    return Vec::new();
                };
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                if start == end {
                    return Vec::new();
                }
                let mut anchors = Vec::new();
                for page in start.page..=end.page {
                    let key = (book.id.clone(), page);
                    let Some(cached) = self.page_cache.get(&key) else {
                        continue;
                    };
                    let texts: Vec<&str> = if !cached.pdf_runs.is_empty() {
                        cached.pdf_runs.iter().map(|r| r.text.as_str()).collect()
                    } else {
                        cached.word_runs.iter().map(|r| r.text.as_str()).collect()
                    };
                    if texts.is_empty() {
                        continue;
                    }
                    let r_start = if page == start.page { start.run_idx } else { 0 };
                    let r_end = if page == end.page {
                        end.run_idx.min(texts.len() - 1)
                    } else {
                        texts.len() - 1
                    };
                    let mut full = String::new();
                    let mut total_chars = 0usize;
                    let mut sel_start_char = None;
                    let mut sel_end_char = 0usize;
                    for (ri, t) in texts.iter().enumerate() {
                        let len = t.chars().count();
                        if ri >= r_start && ri <= r_end {
                            let c0 = if page == start.page && ri == start.run_idx {
                                start.char_idx.min(len)
                            } else {
                                0
                            };
                            let c1 = if page == end.page && ri == end.run_idx {
                                end.char_idx.min(len)
                            } else {
                                len
                            };
                            if c1 > c0 {
                                if sel_start_char.is_none() {
                                    sel_start_char = Some(total_chars + c0);
                                }
                                sel_end_char = total_chars + c1;
                            }
                        }
                        full.push_str(t);
                        total_chars += len;
                    }
                    if let Some(s_char) = sel_start_char
                        && sel_end_char > s_char
                    {
                        let quote = slice_chars(&full, s_char, sel_end_char);
                        if !quote.trim().is_empty() {
                            let before = slice_chars(&full, s_char.saturating_sub(24), s_char);
                            let after = slice_chars(&full, sel_end_char, (sel_end_char + 24).min(total_chars));
                            anchors.push(Anchor {
                                index: page,
                                start: s_char,
                                quote,
                                digest: book.digest.clone(),
                                before,
                                after,
                            });
                        }
                    }
                }
                anchors
            }
            LoadedSource::Flow(_) => {
                let Some(cache) = &self.flow_cache else {
                    return Vec::new();
                };
                let (Some(a), Some(b)) = (self.flow_sel_anchor, self.flow_sel_head) else {
                    return Vec::new();
                };
                let (start, end) = if a <= b { (a, b) } else { (b, a) };
                if start == end || cache.items.is_empty() {
                    return Vec::new();
                }
                let end_idx = end.item_idx.min(cache.items.len() - 1);
                let mut anchors = Vec::new();
                let mut per_chapter: HashMap<usize, (usize, usize, String)> = HashMap::new();
                for (idx, item) in cache.items[..=end_idx].iter().enumerate().skip(start.item_idx) {
                    if item.plain.is_empty() {
                        continue;
                    }
                    let ch = item.chapter_idx;
                    let ch_start_item = cache.chapter_first_item.get(ch).copied().unwrap_or(0);
                    let mut ch_offset = 0usize;
                    for prev in &cache.items[ch_start_item..idx] {
                        if !prev.plain.is_empty() {
                            ch_offset += prev.plain.chars().count() + 1;
                        }
                    }
                    let total_c = item.plain.chars().count();
                    let c0 = if idx == start.item_idx {
                        start.char_idx.min(total_c)
                    } else {
                        0
                    };
                    let c1 = if idx == end.item_idx {
                        end.char_idx.min(total_c)
                    } else {
                        total_c
                    };
                    if c1 > c0 {
                        let frag = slice_chars(&item.plain, c0, c1);
                        let entry = per_chapter
                            .entry(ch)
                            .or_insert_with(|| (ch_offset + c0, ch_offset + c1, String::new()));
                        if !entry.2.is_empty() {
                            entry.2.push('\n');
                        }
                        entry.2.push_str(&frag);
                        entry.1 = ch_offset + c1;
                    }
                }
                let mut keys: Vec<usize> = per_chapter.keys().copied().collect();
                keys.sort_unstable();
                for ch in keys {
                    if let Some((s_char, e_char, quote)) = per_chapter.remove(&ch)
                        && !quote.trim().is_empty()
                    {
                        let ch_full = chapter_plain_from_cache(cache, ch);
                        let ch_len = ch_full.chars().count();
                        let before = slice_chars(&ch_full, s_char.saturating_sub(24), s_char);
                        let after = slice_chars(&ch_full, e_char, (e_char + 24).min(ch_len));
                        anchors.push(Anchor {
                            index: ch,
                            start: s_char,
                            quote,
                            digest: book.digest.clone(),
                            before,
                            after,
                        });
                    }
                }
                anchors
            }
        }
    }

    fn add_highlight_from_selection(&mut self, color: &str) {
        let parts = self.selected_anchors();
        if parts.is_empty() {
            return;
        }
        let unit = parts[0].index;
        let quote_preview = parts
            .iter()
            .map(|p| p.quote.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let digest = self
            .active_book
            .as_ref()
            .map(|b| b.digest.clone())
            .unwrap_or_default();
        if let Some(book) = self.active_book.as_mut() {
            book.state.annotations.push(Annotation {
                id: uuid::Uuid::new_v4().to_string(),
                kind: "highlight".to_owned(),
                color: color.to_owned(),
                text: truncate_str(quote_preview.trim(), 60),
                parts,
                position: Some(Position {
                    index: unit,
                    fraction: self.scroll_fraction,
                    anchor: None,
                }),
                digest,
                missing: false,
            });
            self.save_active_book();
            self.clear_selection();
            self.set_status("已添加高亮。", Some(2));
        }
    }

    fn open_note_editor_from_selection(&mut self) {
        let parts = self.selected_anchors();
        let unit = parts.first().map(|p| p.index).unwrap_or(self.current_unit);
        self.annotation_editor = Some(AnnotationEditorModal {
            existing_id: None,
            kind: "note".to_owned(),
            color: "yellow".to_owned(),
            text: String::new(),
            parts,
            position: Some(Position {
                index: unit,
                fraction: self.scroll_fraction,
                anchor: None,
            }),
        });
    }

    fn open_annotation_editor_for(&mut self, ann: &Annotation) {
        self.annotation_editor = Some(AnnotationEditorModal {
            existing_id: Some(ann.id.clone()),
            kind: ann.kind.clone(),
            color: if ann.color.is_empty() {
                "yellow".to_owned()
            } else {
                ann.color.clone()
            },
            text: ann.text.clone(),
            parts: ann.parts.clone(),
            position: ann.position.clone(),
        });
    }

    fn export_annotations_markdown(&mut self) {
        let Some(book) = &self.active_book else {
            return;
        };
        if book.state.annotations.is_empty() {
            self.set_status("当前文档暂无批注可导出。", Some(2));
            return;
        }
        let is_fixed = matches!(book.source, LoadedSource::Pdf(_) | LoadedSource::Word(_));
        let mut md = format!("# {} · 批注与高亮\n\n", book.title);
        for ann in &book.state.annotations {
            let loc = ann
                .position
                .as_ref()
                .map(|p| {
                    if is_fixed {
                        format!("第 {} 页", p.index + 1)
                    } else {
                        format!("第 {} 章", p.index + 1)
                    }
                })
                .unwrap_or_default();
            let kind_label = match ann.kind.as_str() {
                "bookmark" => "书签",
                "note" => "笔记",
                _ => "高亮",
            };
            md.push_str(&format!("## {kind_label} · {loc}\n\n"));
            if !ann.parts.is_empty() {
                let quote = ann
                    .parts
                    .iter()
                    .map(|p| p.quote.trim())
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if !quote.is_empty() {
                    for line in quote.lines() {
                        md.push_str(&format!("> {line}\n"));
                    }
                    md.push('\n');
                }
            }
            if !ann.text.trim().is_empty() && ann.kind != "highlight" {
                md.push_str(ann.text.trim());
                md.push_str("\n\n");
            }
        }
        let default_name = format!("{}-批注.md", book.title);
        if let Some(path) = rfd::FileDialog::new()
            .add_filter("Markdown", &["md"])
            .set_file_name(&default_name)
            .save_file()
        {
            match fs::write(&path, md) {
                Ok(()) => self.set_status("批注已导出为 Markdown。", Some(3)),
                Err(e) => self.set_status(format!("导出批注失败：{e}"), None),
            }
        }
    }

    fn draw_tabs_bar(&mut self, ui: &mut Ui) {
        self.tab_screen_rects.clear();
        if self.tabs.is_empty() || self.focus_mode {
            return;
        }
        let p = self.palette();
        let dark = self.theme_mode() == ThemeMode::Dark;
        Panel::top("document_tabs")
            .exact_size(36.0)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::symmetric(6, 4)),
            )
            .show(ui, |ui| {
                let avail_w = ui.available_width();
                let arrow_w = 24.0;
                let arrow_gap = 4.0;
                let viewport_w = (avail_w - (arrow_w + arrow_gap) * 2.0).max(40.0);
                let step = (viewport_w * 0.55).max(160.0);
                let can_left = self.tabs_scroll_offset > 0.5;
                let can_right = self.tabs_scroll_offset + 0.5 < self.tabs_scroll_max;

                let mut switch_to: Option<String> = None;
                let mut close_id: Option<String> = None;
                let mut active_rel_span: Option<(f32, f32)> = None;
                let mut new_tab_rects: Vec<(String, Rect)> = Vec::new();
                let mut content_w = 0.0f32;
                let mut tabs_viewport_rect = Rect::NOTHING;

                ui.horizontal_centered(|ui| {
                    ui.spacing_mut().item_spacing.x = arrow_gap;

                    // Left scroll arrow button
                    let (left_rect, left_resp) =
                        ui.allocate_exact_size(Vec2::new(arrow_w, 26.0), Sense::click());
                    let left_resp = left_resp.on_hover_text("向左滚动标签栏");
                    if can_left && left_resp.hovered() {
                        ui.painter().rect_filled(left_rect, 5.0, p.surface);
                    }
                    ui.painter().rect_stroke(
                        left_rect,
                        5.0,
                        Stroke::new(1.0, if can_left { p.line } else { Color32::TRANSPARENT }),
                        StrokeKind::Inside,
                    );
                    draw_chevron(
                        ui.painter(),
                        left_rect.center(),
                        true,
                        if can_left {
                            p.ink
                        } else {
                            Color32::from_rgba_unmultiplied(p.muted.r(), p.muted.g(), p.muted.b(), 110)
                        },
                    );
                    if can_left && left_resp.clicked() {
                        self.tabs_scroll_offset = (self.tabs_scroll_offset - step).max(0.0);
                    }

                    // Scrollable tab strip with scrollbar completely hidden
                    let scroll_out = ui
                        .allocate_ui_with_layout(
                            Vec2::new(viewport_w, 28.0),
                            egui::Layout::left_to_right(Align::Center),
                            |ui| {
                                ScrollArea::horizontal()
                                    .id_salt("document_tabs_scroll")
                                    .scroll_bar_visibility(
                                        egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                    )
                                    .horizontal_scroll_offset(self.tabs_scroll_offset)
                                    .show(ui, |ui| {
                                        let inner = ui.horizontal(|ui| {
                                            let content_origin_x = ui.cursor().left();
                                            let active_id =
                                                self.active_book.as_ref().map(|b| b.id.clone());
                                            for tab in &self.tabs {
                                                let is_active =
                                                    active_id.as_deref() == Some(tab.id.as_str());
                                                let sleeping = self.is_tab_sleeping(&tab.id);
                                                let frame = Frame::new()
                                                    .fill(if is_active { p.surface } else { p.side })
                                                    .stroke(Stroke::new(
                                                        1.0,
                                                        if is_active { p.accent } else { p.line },
                                                    ))
                                                    .corner_radius(CornerRadius::same(6))
                                                    .inner_margin(Margin::symmetric(8, 3));
                                                let tab_resp = frame
                                                    .show(ui, |ui| {
                                                        ui.horizontal(|ui| {
                                                            ui.spacing_mut().item_spacing.x = 5.0;
                                                            if sleeping {
                                                                let (leaf_rect, _) =
                                                                    ui.allocate_exact_size(
                                                                        Vec2::new(13.0, 13.0),
                                                                        Sense::hover(),
                                                                    );
                                                                draw_eco_leaf_icon(
                                                                    ui.painter(),
                                                                    leaf_rect,
                                                                    dark,
                                                                );
                                                            }
                                                            let text_color = if sleeping {
                                                                Color32::from_rgba_unmultiplied(
                                                                    p.muted.r(),
                                                                    p.muted.g(),
                                                                    p.muted.b(),
                                                                    195,
                                                                )
                                                            } else {
                                                                p.ink
                                                            };
                                                            let label_text = RichText::new(
                                                                truncate_str(&tab.title, 22),
                                                            )
                                                            .color(text_color);
                                                            let tooltip = if sleeping {
                                                                format!(
                                                                    "{}（已休眠以节省内存，点击恢复）",
                                                                    tab.title
                                                                )
                                                            } else {
                                                                tab.title.clone()
                                                            };
                                                            let btn = ui
                                                                .add(
                                                                    egui::Button::new(label_text)
                                                                        .frame(false),
                                                                )
                                                                .on_hover_text(tooltip);
                                                            if btn.clicked() && !is_active {
                                                                switch_to = Some(tab.id.clone());
                                                            }
                                                            if btn.middle_clicked() {
                                                                close_id = Some(tab.id.clone());
                                                            }
                                                            let close_text =
                                                                RichText::new("×").color(
                                                                    if sleeping {
                                                                        text_color
                                                                    } else {
                                                                        p.muted
                                                                    },
                                                                );
                                                            if ui
                                                                .add(
                                                                    egui::Button::new(close_text)
                                                                        .frame(false),
                                                                )
                                                                .on_hover_text("关闭标签页")
                                                                .clicked()
                                                            {
                                                                close_id = Some(tab.id.clone());
                                                            }
                                                        });
                                                    })
                                                    .response;
                                                let tab_rect = tab_resp.rect;
                                                new_tab_rects.push((tab.id.clone(), tab_rect));
                                                if is_active {
                                                    active_rel_span = Some((
                                                        tab_rect.left() - content_origin_x,
                                                        tab_rect.right() - content_origin_x,
                                                    ));
                                                }
                                            }
                                        });
                                        inner.response.rect.width()
                                    })
                            },
                        )
                        .inner;
                    content_w = scroll_out.content_size.x.max(scroll_out.inner);
                    tabs_viewport_rect = scroll_out.inner_rect;

                    // Right scroll arrow button
                    let (right_rect, right_resp) =
                        ui.allocate_exact_size(Vec2::new(arrow_w, 26.0), Sense::click());
                    let right_resp = right_resp.on_hover_text("向右滚动标签栏");
                    if can_right && right_resp.hovered() {
                        ui.painter().rect_filled(right_rect, 5.0, p.surface);
                    }
                    ui.painter().rect_stroke(
                        right_rect,
                        5.0,
                        Stroke::new(1.0, if can_right { p.line } else { Color32::TRANSPARENT }),
                        StrokeKind::Inside,
                    );
                    draw_chevron(
                        ui.painter(),
                        right_rect.center(),
                        false,
                        if can_right {
                            p.ink
                        } else {
                            Color32::from_rgba_unmultiplied(p.muted.r(), p.muted.g(), p.muted.b(), 110)
                        },
                    );
                    if can_right && right_resp.clicked() {
                        self.tabs_scroll_offset =
                            (self.tabs_scroll_offset + step).min(self.tabs_scroll_max);
                    }
                });

                for (id, rect) in new_tab_rects {
                    self.tab_screen_rects.insert(id, rect);
                }

                self.tabs_scroll_max = (content_w - viewport_w).max(0.0);
                self.tabs_scroll_offset =
                    self.tabs_scroll_offset.clamp(0.0, self.tabs_scroll_max);

                // Support mouse wheel scrolling when hovering the tab bar
                if !self.is_modal_open()
                    && self.tabs_scroll_max > 0.5
                    && ui
                        .ctx()
                        .pointer_latest_pos()
                        .is_some_and(|pos| tabs_viewport_rect.contains(pos))
                {
                    let (dx, dy) = ui
                        .ctx()
                        .input(|i| (i.smooth_scroll_delta.x, i.smooth_scroll_delta.y));
                    let delta = if dx.abs() > dy.abs() { -dx } else { -dy };
                    if delta.abs() > 0.1 {
                        self.tabs_scroll_offset = (self.tabs_scroll_offset + delta)
                            .clamp(0.0, self.tabs_scroll_max);
                        ui.ctx().request_repaint();
                    }
                }

                // Auto-scroll active tab into view when switched programmatically or opened externally
                if self.scroll_active_tab_into_view {
                    if let Some((rel_left, rel_right)) = active_rel_span {
                        let old = self.tabs_scroll_offset;
                        if rel_left < self.tabs_scroll_offset {
                            self.tabs_scroll_offset = (rel_left - 6.0).max(0.0);
                        } else if rel_right > self.tabs_scroll_offset + viewport_w {
                            self.tabs_scroll_offset = (rel_right - viewport_w + 6.0)
                                .clamp(0.0, self.tabs_scroll_max);
                        }
                        if (self.tabs_scroll_offset - old).abs() > 0.5 {
                            ui.ctx().request_repaint();
                        }
                        self.scroll_active_tab_into_view = false;
                    } else if self.active_book.is_some() {
                        ui.ctx().request_repaint();
                    } else {
                        self.scroll_active_tab_into_view = false;
                    }
                }

                if let Some(id) = close_id {
                    self.close_tab(&id);
                } else if let Some(id) = switch_to {
                    self.switch_to_tab(&id);
                }
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
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(p.side)
                    .inner_margin(Margin::same(10)),
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
                            let fallback_idx = entries.iter().position(|e| e.index == self.current_unit);
                            for (oi, entry) in entries.iter().enumerate() {
                                let active = if let Some(sel) = self.active_outline_idx
                                    && entries.get(sel).is_some_and(|e| e.index == self.current_unit)
                                {
                                    sel == oi
                                } else {
                                    fallback_idx == Some(oi)
                                };
                                let text = if active {
                                    RichText::new(&entry.title).strong().color(p.accent)
                                } else {
                                    RichText::new(&entry.title)
                                };
                                if ui.selectable_label(active, text).clicked() {
                                    self.pending_jump = Some(PendingJump {
                                        chapter: entry.index,
                                        fragment: entry.fragment.clone(),
                                        fraction: 0.0,
                                    });
                                    self.current_unit = entry.index;
                                    self.active_outline_idx = Some(oi);
                                }
                            }
                        }
                    }
                    SidebarTab::Notes => {
                        let mut remove_idx = None;
                        let mut edit_ann: Option<Annotation> = None;
                        let mut jump_to = None;
                        let mut export_md = false;
                        if let Some(book) = &self.active_book {
                            if book.state.annotations.is_empty() {
                                ui.label(RichText::new("暂无书签、高亮或批注").color(p.muted));
                                ui.add_space(4.0);
                                ui.label(
                                    RichText::new("提示：拖动选中正文即可一键添加四色高亮或笔记。")
                                        .size(12.5)
                                        .color(p.muted),
                                );
                            } else {
                                ui.horizontal(|ui| {
                                    if ui.button("导出 Markdown").clicked() {
                                        export_md = true;
                                    }
                                });
                                ui.add_space(6.0);
                                let is_fixed = matches!(
                                    book.source,
                                    LoadedSource::Pdf(_) | LoadedSource::Word(_)
                                );
                                for (idx, ann) in book.state.annotations.iter().enumerate() {
                                    let card = Frame::new()
                                        .fill(p.surface)
                                        .stroke(Stroke::new(1.0, p.line))
                                        .corner_radius(CornerRadius::same(7))
                                        .inner_margin(Margin::same(8));
                                    card.show(ui, |ui| {
                                        ui.set_width(ui.available_width());
                                        ui.horizontal(|ui| {
                                            let dot_color = if ann.kind == "bookmark" {
                                                p.accent
                                            } else {
                                                highlight_swatch_color(&ann.color)
                                            };
                                            let (dot_rect, _) = ui.allocate_exact_size(
                                                Vec2::splat(10.0),
                                                Sense::hover(),
                                            );
                                            ui.painter().circle_filled(
                                                dot_rect.center(),
                                                4.5,
                                                dot_color,
                                            );
                                            let kind_txt = match ann.kind.as_str() {
                                                "bookmark" => "书签",
                                                "note" => "笔记",
                                                _ => "高亮",
                                            };
                                            let loc_txt = ann
                                                .position
                                                .as_ref()
                                                .map(|pos| {
                                                    if is_fixed {
                                                        format!("第 {} 页", pos.index + 1)
                                                    } else {
                                                        format!("第 {} 章", pos.index + 1)
                                                    }
                                                })
                                                .unwrap_or_default();
                                            if ui
                                                .add(
                                                    egui::Button::new(
                                                        RichText::new(format!(
                                                            "{kind_txt} · {loc_txt}"
                                                        ))
                                                        .size(12.5)
                                                        .color(p.muted),
                                                    )
                                                    .frame(false),
                                                )
                                                .clicked()
                                            {
                                                if let Some(pos) = &ann.position {
                                                    jump_to = Some((pos.index, pos.fraction));
                                                } else if let Some(first) = ann.parts.first() {
                                                    jump_to = Some((first.index, 0.0));
                                                }
                                            }
                                            ui.with_layout(
                                                egui::Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if ui
                                                        .add(egui::Button::new("×").frame(false))
                                                        .on_hover_text("删除")
                                                        .clicked()
                                                    {
                                                        remove_idx = Some(idx);
                                                    }
                                                    if ui
                                                        .add(
                                                            egui::Button::new(
                                                                RichText::new("编辑").size(12.0),
                                                            )
                                                            .frame(false),
                                                        )
                                                        .clicked()
                                                    {
                                                        edit_ann = Some(ann.clone());
                                                    }
                                                },
                                            );
                                        });
                                        if let Some(first_part) = ann.parts.first()
                                            && !first_part.quote.trim().is_empty()
                                        {
                                            let q = truncate_str(first_part.quote.trim(), 48);
                                            if ui
                                                .add(
                                                    egui::Label::new(
                                                        RichText::new(format!("“{q}”"))
                                                            .size(12.5)
                                                            .color(p.muted),
                                                    )
                                                    .wrap()
                                                    .sense(Sense::click()),
                                                )
                                                .clicked()
                                            {
                                                if let Some(pos) = &ann.position {
                                                    jump_to = Some((pos.index, pos.fraction));
                                                } else {
                                                    jump_to = Some((first_part.index, 0.0));
                                                }
                                            }
                                        }
                                        if !ann.text.trim().is_empty()
                                            && ann.kind != "highlight"
                                            && ui
                                                .add(
                                                    egui::Label::new(
                                                        RichText::new(truncate_str(
                                                            ann.text.trim(),
                                                            80,
                                                        ))
                                                        .color(p.ink),
                                                    )
                                                    .wrap()
                                                    .sense(Sense::click()),
                                                )
                                                .clicked()
                                            && let Some(pos) = &ann.position
                                        {
                                            jump_to = Some((pos.index, pos.fraction));
                                        }
                                    });
                                    ui.add_space(4.0);
                                }
                            }
                        } else {
                            ui.label(RichText::new("请先打开文档").color(p.muted));
                        }
                        if export_md {
                            self.export_annotations_markdown();
                        }
                        if let Some(ann) = edit_ann {
                            self.open_annotation_editor_for(&ann);
                        }
                        if let Some(idx) = remove_idx
                            && let Some(book) = self.active_book.as_mut()
                            && idx < book.state.annotations.len()
                        {
                            book.state.annotations.remove(idx);
                            self.save_active_book();
                            self.sync_auto_sidebar(false);
                        }
                        if let Some((unit, fraction)) = jump_to {
                            self.pending_jump = Some(PendingJump {
                                chapter: unit,
                                fragment: String::new(),
                                fraction,
                            });
                            self.current_unit = unit;
                        }
                    }
                });
            });
    }

    fn draw_bottom_bar(&mut self, ui: &mut Ui) {
        self.smart_invert_btn_rect = None;
        if self.focus_mode {
            return;
        }
        let p = self.palette();
        let dark = self.theme_mode() == ThemeMode::Dark;
        let uses_smart_invert = self.active_book_uses_smart_invert();
        Panel::bottom("status_footer")
            .exact_size(38.0)
            .show_separator_line(true)
            .frame(
                Frame::new()
                    .fill(p.surface)
                    .inner_margin(Margin::symmetric(12, 4)),
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
                        "就绪".to_owned()
                    };
                    ui.label(RichText::new(loc).color(p.muted));

                    if dark && uses_smart_invert {
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
                        let btn_resp = ui.add(btn);
                        self.smart_invert_btn_rect = Some(btn_resp.rect);
                        if btn_resp.clicked() {
                            self.settings.smart_invert = !self.settings.smart_invert;
                            self.page_cache.clear();
                            self.save_settings();
                            if !self.settings.smart_invert {
                                self.smart_invert_hint_open = false;
                            }
                        }
                    }

                    let is_fixed = self.active_book.as_ref().is_some_and(|b| {
                        matches!(b.source, LoadedSource::Pdf(_) | LoadedSource::Word(_))
                    });
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if is_fixed {
                            if ui.button("+").clicked() {
                                let cur = self.fixed_zoom.unwrap_or(1.0);
                                self.fixed_zoom = Some((cur * 1.15).min(4.5));
                            }
                            let pct = (self.fixed_zoom.unwrap_or(1.0) * 100.0).round() as i32;
                            if ui
                                .button(format!("{pct}%"))
                                .on_hover_text("点击恢复适合页宽 (100%)")
                                .clicked()
                            {
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
                                    fragment: String::new(),
                                    fraction: 0.0,
                                });
                            }
                            if ui.button("‹").clicked() && self.current_unit > 0 {
                                let prev = self.current_unit - 1;
                                self.current_unit = prev;
                                self.pending_jump = Some(PendingJump {
                                    chapter: prev,
                                    fragment: String::new(),
                                    fraction: 0.0,
                                });
                            }
                        } else {
                            if ui.button("+").clicked() {
                                self.settings.font_size =
                                    (self.settings.font_size + 1.0).min(34.0);
                                self.flow_cache = None;
                                self.save_settings();
                            }
                            let pct =
                                (self.settings.font_size / 19.0 * 100.0).round() as i32;
                            if ui
                                .button(format!("{pct}%"))
                                .on_hover_text("点击恢复默认缩放 (100%，字号 19px)")
                                .clicked()
                            {
                                self.settings.font_size = 19.0;
                                self.flow_cache = None;
                                self.save_settings();
                            }
                            if ui.button("−").clicked() {
                                self.settings.font_size =
                                    (self.settings.font_size - 1.0).max(13.0);
                                self.flow_cache = None;
                                self.save_settings();
                            }
                        }
                    });
                });
            });
    }

    fn draw_welcome(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let mut open_repo = false;
        let mut open_files = false;
        let mut open_target = None;
        let mut del_target = None;
        let mut clear_recent = false;
        ScrollArea::vertical().show(ui, |ui| {
            let avail_w = ui.available_width();
            let col_w = (avail_w - 48.0).clamp(320.0, 680.0);
            let side_pad = ((avail_w - col_w) * 0.5).max(16.0);
            ui.add_space(28.0);
            ui.horizontal(|ui| {
                ui.add_space(side_pad);
                ui.allocate_ui_with_layout(
                    Vec2::new(col_w, 0.0),
                    egui::Layout::top_down(Align::LEFT),
                    |ui| {
                        ui.set_width(col_w);
                        ui.vertical_centered(|ui| {
                            if let Some(logo) = &self.logo_texture {
                                ui.add(egui::Image::new((logo.id(), Vec2::splat(56.0))));
                                ui.add_space(10.0);
                            }
                            ui.label(
                                RichText::new("HayakuReader")
                                    .size(32.0)
                                    .strong()
                                    .color(p.ink),
                            );
                            ui.add_space(4.0);
                            ui.label(
                                RichText::new(format!(
                                    "v{} · 纯 Rust 原生本地阅读器",
                                    platform::VERSION
                                ))
                                .size(13.5)
                                .color(p.muted),
                            );
                        });
                        ui.add_space(16.0);

                        if draw_welcome_intro_markdown(ui, INTRO_ZH_MD, p) {
                            open_repo = true;
                        }

                        ui.add_space(18.0);
                        ui.vertical_centered(|ui| {
                            if ui
                                .add_sized(
                                    [156.0, 38.0],
                                    egui::Button::new(
                                        RichText::new("打开文件")
                                            .size(15.0)
                                            .strong()
                                            .color(p.accent_ink),
                                    )
                                    .fill(p.accent),
                                )
                                .clicked()
                            {
                                open_files = true;
                            }
                        });

                        if !self.recent.is_empty() {
                            ui.add_space(22.0);
                            Frame::new()
                                .fill(p.surface)
                                .stroke(Stroke::new(1.0, p.line))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(Margin::same(14))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("最近阅读").size(15.0).strong());
                                        ui.with_layout(
                                            egui::Layout::right_to_left(Align::Center),
                                            |ui| {
                                                if ui.button("清空记录").clicked() {
                                                    clear_recent = true;
                                                }
                                            },
                                        );
                                    });
                                    ui.separator();
                                    for rec in self.recent.iter().take(12) {
                                        let name = rec
                                            .path
                                            .file_name()
                                            .map(|s| s.to_string_lossy().into_owned())
                                            .unwrap_or_else(|| rec.path.display().to_string());
                                        ui.horizontal(|ui| {
                                            if ui
                                                .button(truncate_str(&name, 52))
                                                .on_hover_text(rec.path.display().to_string())
                                                .clicked()
                                            {
                                                open_target = Some(rec.path.clone());
                                            }
                                            ui.with_layout(
                                                egui::Layout::right_to_left(Align::Center),
                                                |ui| {
                                                    if ui.button("×").clicked() {
                                                        del_target = Some(rec.id.clone());
                                                    }
                                                },
                                            );
                                        });
                                    }
                                });
                        }
                        ui.add_space(36.0);
                    },
                );
            });
        });
        if open_repo && let Err(e) = platform::shell_open(platform::REPO) {
            self.set_status(e, Some(4));
        }
        if open_files {
            self.pick_and_open_files();
        }
        if clear_recent {
            let _ = storage::clear_recent_entries(&self.data, &mut self.recent);
        }
        if let Some(id) = del_target {
            let _ = storage::delete_recent_entry(&self.data, &mut self.recent, &id);
        }
        if let Some(path) = open_target {
            self.open_path(&path);
        }
    }

    fn draw_fixed_viewer(&mut self, ui: &mut Ui) {
        let smart_invert = self.is_smart_invert();
        let p = self.palette();
        let avail_w = (ui.available_width() - 32.0).max(200.0);
        let ppp = ui.ctx().pixels_per_point();
        let (book_id, page_sizes) = match self.active_book.as_ref() {
            Some(book) => match &book.source {
                LoadedSource::Pdf(pdf) => (book.id.clone(), pdf.page_sizes().to_vec()),
                LoadedSource::Word(w) => (book.id.clone(), w.metadata.pages.clone()),
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
        if let Some(target_y) = self.pending_scroll_y.take() {
            scroll = scroll.vertical_scroll_offset(target_y.max(0.0));
        } else if let Some(jump) = self.pending_jump.take()
            && let Some(&(top, _, h)) = page_tops.get(jump.chapter)
        {
            scroll = scroll.vertical_scroll_offset((top - 12.0 + h * jump.fraction).max(0.0));
        }

        let mut clicked_external: Option<String> = None;
        let mut clicked_page: Option<usize> = None;
        let mut requested_menu: Option<ImageContextMenu> = None;
        let mut clicked_annotation_id: Option<String> = None;
        let mut sel_bounds: Option<Rect> = None;

        let dark = self.theme_mode() == ThemeMode::Dark;
        let page_annotations: Vec<Annotation> = self
            .active_book
            .as_ref()
            .map(|b| b.state.annotations.clone())
            .unwrap_or_default();

        let modal_open = self.is_modal_open();
        let has_ctx_menu = self.context_menu.is_some();
        let pointer_pos = (!modal_open)
            .then(|| ui.ctx().pointer_latest_pos())
            .flatten();
        let primary_pressed =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_pressed());
        let primary_down =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_down());
        let primary_released =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_released());
        let was_dragging = self.fixed_sel_dragging;
        if !primary_down {
            self.fixed_sel_dragging = false;
        }

        let scroll_out = scroll.show_viewport(ui, |ui, viewport| {
            let clip = ui.clip_rect();
            let view_pointer_pos = pointer_pos
                .filter(|&pos| clip.contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()));
            let drag_pointer_pos = pointer_pos.filter(|&pos| clip.contains(pos));
            let hit_pointer_pos = if self.fixed_sel_dragging {
                drag_pointer_pos
            } else {
                view_pointer_pos
            };
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
                                let pdf_runs = pdf.page_text_runs(idx);
                                self.use_tick += 1;
                                self.page_cache.insert(
                                    cache_key.clone(),
                                    CachedPageTexture {
                                        scale_key,
                                        smart_invert,
                                        links,
                                        pdf_runs,
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
                                        pdf_runs: Vec::new(),
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

                    // Paint saved highlights & notes on this page
                    let page_hl_rects = paint_fixed_page_annotations(
                        ui,
                        idx,
                        page_rect,
                        factor,
                        &cached.pdf_runs,
                        &cached.word_runs,
                        &page_annotations,
                        dark,
                    );
                    if primary_released
                        && (!was_dragging || self.fixed_sel_anchor == self.fixed_sel_head)
                        && let Some(pos) = view_pointer_pos
                        && let Some((ann_id, _)) =
                            page_hl_rects.iter().find(|(_, r)| r.contains(pos))
                    {
                        clicked_annotation_id = Some(ann_id.clone());
                    }
                    if view_pointer_pos.is_some_and(|pos| {
                        page_hl_rects.iter().any(|(_, r)| r.contains(pos))
                    }) && !self.fixed_sel_dragging
                    {
                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                    }

                    // Paint selected text runs
                    if self.fixed_all_selected {
                        for run in &cached.pdf_runs {
                            let rr = Rect::from_min_size(
                                Pos2::new(
                                    page_rect.left() + run.rect_pt[0] * factor,
                                    page_rect.top() + run.rect_pt[1] * factor,
                                ),
                                Vec2::new(run.rect_pt[2] * factor, run.rect_pt[3] * factor),
                            );
                            ui.painter().rect_filled(rr, 1.0, p.selection);
                        }
                        for run in &cached.word_runs {
                            let rr = Rect::from_min_size(
                                Pos2::new(
                                    page_rect.left() + run.left * factor,
                                    page_rect.top() + run.top * factor,
                                ),
                                Vec2::new(run.width * factor, (run.size * 1.2) * factor),
                            );
                            ui.painter().rect_filled(rr, 1.0, p.selection);
                        }
                    } else if let (Some(a), Some(b)) = (self.fixed_sel_anchor, self.fixed_sel_head) {
                        let (s, e) = if a <= b { (a, b) } else { (b, a) };
                        if s != e && idx >= s.page && idx <= e.page {
                            let run_len = cached.pdf_runs.len().max(cached.word_runs.len());
                            for ri in 0..run_len {
                                let (rr, text_chars) = if !cached.pdf_runs.is_empty() {
                                    let run = &cached.pdf_runs[ri];
                                    (
                                        Rect::from_min_size(
                                            Pos2::new(
                                                page_rect.left() + run.rect_pt[0] * factor,
                                                page_rect.top() + run.rect_pt[1] * factor,
                                            ),
                                            Vec2::new(run.rect_pt[2] * factor, run.rect_pt[3] * factor),
                                        ),
                                        run.text.chars().count().max(1),
                                    )
                                } else {
                                    let run = &cached.word_runs[ri];
                                    (
                                        Rect::from_min_size(
                                            Pos2::new(
                                                page_rect.left() + run.left * factor,
                                                page_rect.top() + run.top * factor,
                                            ),
                                            Vec2::new(run.width * factor, (run.size * 1.2) * factor),
                                        ),
                                        run.text.chars().count().max(1),
                                    )
                                };
                                let in_range = (idx > s.page || ri >= s.run_idx)
                                    && (idx < e.page || ri <= e.run_idx);
                                if !in_range {
                                    continue;
                                }
                                let c0 = if idx == s.page && ri == s.run_idx {
                                    s.char_idx.min(text_chars)
                                } else {
                                    0
                                };
                                let c1 = if idx == e.page && ri == e.run_idx {
                                    e.char_idx.min(text_chars)
                                } else {
                                    text_chars
                                };
                                if c1 > c0 {
                                    let f0 = c0 as f32 / text_chars as f32;
                                    let f1 = c1 as f32 / text_chars as f32;
                                    let sub = Rect::from_min_max(
                                        Pos2::new(rr.left() + rr.width() * f0, rr.top()),
                                        Pos2::new(rr.left() + rr.width() * f1, rr.bottom()),
                                    );
                                    ui.painter().rect_filled(sub, 1.0, p.selection);
                                    let vis_sub = sub.intersect(clip);
                                    if vis_sub.is_positive() {
                                        sel_bounds = Some(match sel_bounds {
                                            Some(acc) => acc.union(vis_sub),
                                            None => vis_sub,
                                        });
                                    }
                                }
                            }
                        }
                    }

                    // Pointer drag selection over runs
                    if let Some(pos) = hit_pointer_pos
                        && page_rect.contains(pos)
                    {
                        let hit = hit_test_fixed_page(
                            idx,
                            pos,
                            page_rect,
                            factor,
                            &cached.pdf_runs,
                            &cached.word_runs,
                        );
                        if view_pointer_pos.is_some() && hit.is_some() {
                            ui.ctx().set_cursor_icon(CursorIcon::Text);
                        }
                        if primary_pressed && view_pointer_pos.is_some() {
                            self.fixed_all_selected = false;
                            if let Some(h) = hit {
                                self.fixed_sel_anchor = Some(h);
                                self.fixed_sel_head = Some(h);
                                self.fixed_sel_dragging = true;
                            } else {
                                self.fixed_sel_anchor = None;
                                self.fixed_sel_head = None;
                            }
                        } else if self.fixed_sel_dragging && primary_down && let Some(h) = hit {
                            self.fixed_sel_head = Some(h);
                        }
                    }

                    // Interactive links overlay
                    if !modal_open {
                        for link in &cached.links {
                            let lr = Rect::from_min_size(
                                Pos2::new(
                                    page_rect.left() + link.rect_pt[0] * factor,
                                    page_rect.top() + link.rect_pt[1] * factor,
                                ),
                                Vec2::new(link.rect_pt[2] * factor, link.rect_pt[3] * factor),
                            )
                            .intersect(clip);
                            if !lr.is_positive() {
                                continue;
                            }
                            let resp = ui.interact(
                                lr,
                                ui.scope_id()
                                    .with(("pdf-link", idx, lr.min.x as i32, lr.min.y as i32)),
                                Sense::click(),
                            );
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
                                } else if let Some(dest_p) = link.dest_page {
                                    clicked_page = Some(dest_p);
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
                                )
                                .intersect(clip);
                                if !lr.is_positive() {
                                    continue;
                                }
                                let resp = ui.interact(
                                    lr,
                                    ui.scope_id().with(("wlink", idx, ri)),
                                    Sense::click(),
                                );
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
                    }

                    let page_resp = if modal_open {
                        None
                    } else {
                        let visible_page = page_rect.intersect(clip);
                        visible_page.is_positive().then(|| {
                            ui.interact(
                                visible_page,
                                ui.scope_id().with(("fixed-page-ctx", idx)),
                                Sense::click(),
                            )
                        })
                    };
                    if let Some(page_resp) = page_resp
                        && page_resp.secondary_clicked()
                        && let Some(pointer) = view_pointer_pos
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
                                    let hit_box = word.metadata.images.get(idx).and_then(|boxes| {
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
                                    if let Ok((rw, rh, rgba)) =
                                        word.render_rgba(idx, Some(desired_scale))
                                    {
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
                                        let wt = word_runs_plain(&runs);
                                        if !wt.trim().is_empty() {
                                            copy_text = Some(wt);
                                        }
                                    }
                                }
                                LoadedSource::Flow(_) => {}
                            }
                        }
                        let sel = self.selected_fixed_text();
                        requested_menu = Some(ImageContextMenu {
                            pos: pointer,
                            label: kind_label.to_owned(),
                            filename,
                            png_bytes: extracted_png,
                            selected_text: (!sel.is_empty()).then_some(sel),
                            extra_text: copy_text,
                        });
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

        self.last_scroll_y = scroll_out.state.clamped_offset().y;
        self.last_viewport_h = scroll_out.inner_rect.height();
        self.last_content_h = scroll_out.content_size.y;
        self.last_selection_rect = sel_bounds;

        if let Some(ann_id) = clicked_annotation_id
            && let Some(ann) = self
                .active_book
                .as_ref()
                .and_then(|b| b.state.annotations.iter().find(|a| a.id == ann_id))
                .cloned()
        {
            self.open_annotation_editor_for(&ann);
        }
        if let Some(url) = clicked_external {
            self.handle_link_click(&LinkTarget::External(url));
        }
        if let Some(p) = clicked_page {
            self.pending_jump = Some(PendingJump {
                chapter: p,
                fragment: String::new(),
                fraction: 0.0,
            });
            self.current_unit = p;
        }
        if let Some(menu) = requested_menu {
            self.context_menu = Some(menu);
        }
    }

    fn ensure_flow_cache(&mut self, ui: &Ui, measure: f32) {
        let Some(book) = &self.active_book else {
            self.flow_cache = None;
            return;
        };
        let LoadedSource::Flow(flow) = &book.source else {
            return;
        };
        let is_code_mode = book.state.view_mode.as_deref() == Some("code");
        let font_size = self.settings.font_size;
        let line_height = self.settings.line_height;
        let dark = self.theme_mode() == ThemeMode::Dark;
        let key = FlowCacheKey {
            book_id: book.id.clone(),
            is_code_mode,
            width_px: (measure.round() as i32).max(120),
            font_size_tenths: (font_size * 10.0).round() as u32,
            line_height_hundredths: (line_height * 100.0).round() as u32,
            dark,
        };
        if self
            .flow_cache
            .as_ref()
            .is_some_and(|c| c.key == key)
        {
            return;
        }

        let mut items = Vec::new();
        let mut chapter_first_item = Vec::with_capacity(flow.chapters.len());
        let mut anchor_to_item = HashMap::new();

        if is_code_mode {
            let lang = match book.kind {
                "xml" | "html" => CodeLang::XmlHtml,
                "markdown" => CodeLang::Markdown,
                _ => CodeLang::Code,
            };
            let row_h = ((font_size * 0.88).max(12.0) * 1.48).ceil();
            for (ci, ch) in flow.chapters.iter().enumerate() {
                chapter_first_item.push(items.len());
                if ci > 0 {
                    items.push(FlowItemEntry {
                        chapter_idx: ci,
                        source: FlowSourceRef::Separator,
                        height: 28.0,
                        visual: None,
                        plain: String::new(),
                    });
                }
                for (ln, line) in ch.raw_source.split('\n').enumerate() {
                    let clean_line = line.strip_suffix('\r').unwrap_or(line).to_owned();
                    items.push(FlowItemEntry {
                        chapter_idx: ci,
                        source: FlowSourceRef::CodeLine {
                            line_no: ln + 1,
                            text: clean_line.clone(),
                            lang,
                        },
                        height: row_h,
                        visual: None,
                        plain: clean_line,
                    });
                }
            }
        } else {
            let chars_per_line = (measure / (font_size * 0.92)).max(12.0);
            let line_px = font_size * line_height;
            for (ci, ch) in flow.chapters.iter().enumerate() {
                chapter_first_item.push(items.len());
                let block_base = items.len();
                if ci > 0 {
                    items.push(FlowItemEntry {
                        chapter_idx: ci,
                        source: FlowSourceRef::Separator,
                        height: 36.0,
                        visual: None,
                        plain: String::new(),
                    });
                }
                let content_start = items.len();
                for (bi, block) in ch.blocks.iter().enumerate() {
                    let plain = block_plain_text(block);
                    let est = estimate_block_height(block, &plain, chars_per_line, line_px, font_size);
                    items.push(FlowItemEntry {
                        chapter_idx: ci,
                        source: FlowSourceRef::Block {
                            chapter_idx: ci,
                            block_idx: bi,
                        },
                        height: est,
                        visual: None,
                        plain,
                    });
                }
                if content_start < items.len() {
                    for (frag, &b_idx) in &ch.anchors {
                        let mapped = (content_start + b_idx).min(items.len() - 1);
                        anchor_to_item.insert((ci, frag.clone()), mapped);
                    }
                } else {
                    anchor_to_item.insert((ci, String::new()), block_base);
                }
            }
        }

        let _ = ui;
        self.flow_cache = Some(FlowLayoutCache {
            key,
            items,
            chapter_first_item,
            anchor_to_item,
        });
        self.flow_sel_anchor = None;
        self.flow_sel_head = None;
    }

    fn draw_flow_viewer(&mut self, ui: &mut Ui) {
        let p = self.palette();
        let mode = self.theme_mode();
        let font_size = self.settings.font_size;
        let line_height = self.settings.line_height;
        let avail_w = ui.available_width().max(260.0);
        let is_code_mode = self
            .active_book
            .as_ref()
            .and_then(|b| b.state.view_mode.as_deref())
            == Some("code");
        let max_measure = (avail_w - 32.0).max(320.0);
        let min_measure = 360.0_f32.min(max_measure);
        let measure = if is_code_mode {
            (avail_w - 64.0).clamp(320.0, 1180.0)
        } else {
            self.settings.text_width.clamp(min_measure, max_measure)
        };

        let (book_id, flow) = match self.active_book.as_ref() {
            Some(book) => match &book.source {
                LoadedSource::Flow(flow) => (book.id.clone(), Arc::clone(flow)),
                _ => return,
            },
            None => return,
        };

        self.ensure_flow_cache(ui, measure);
        if self.flow_cache.is_none() {
            return;
        }

        // Resolve PendingJump into scroll offset before ScrollArea renders
        let mut initial_scroll_offset: Option<f32> = self.pending_scroll_y.take();
        if initial_scroll_offset.is_none()
            && let Some(jump) = self.pending_jump.take()
            && let Some(cache) = self.flow_cache.as_mut()
        {
            let target_item = if !jump.fragment.is_empty() {
                cache
                    .anchor_to_item
                    .get(&(jump.chapter, jump.fragment.clone()))
                    .copied()
                    .or_else(|| cache.chapter_first_item.get(jump.chapter).copied())
            } else {
                cache.chapter_first_item.get(jump.chapter).copied()
            };
            if let Some(ti) = target_item {
                let mut offset_y: f32 = 24.0
                    + cache.items[..ti.min(cache.items.len())]
                        .iter()
                        .map(|it| it.height)
                        .sum::<f32>();
                if jump.fraction > 0.0 && jump.fragment.is_empty() {
                    let ch_start = cache
                        .chapter_first_item
                        .get(jump.chapter)
                        .copied()
                        .unwrap_or(0);
                    let ch_end = cache
                        .chapter_first_item
                        .get(jump.chapter + 1)
                        .copied()
                        .unwrap_or(cache.items.len());
                    let ch_h: f32 = cache.items[ch_start..ch_end].iter().map(|it| it.height).sum();
                    offset_y += ch_h * jump.fraction.clamp(0.0, 1.0);
                }
                initial_scroll_offset = Some((offset_y - 16.0).max(0.0));
            }
        }

        let mut scroll = ScrollArea::vertical().id_salt("flow_scroll");
        if let Some(target_y) = initial_scroll_offset {
            scroll = scroll.vertical_scroll_offset(target_y);
        }

        let mut clicked_link: Option<LinkTarget> = None;
        let mut requested_menu: Option<ImageContextMenu> = None;
        let mut clicked_annotation_id: Option<String> = None;
        let mut sel_bounds: Option<Rect> = None;

        let dark = mode == ThemeMode::Dark;
        let flow_annotations: Vec<Annotation> = self
            .active_book
            .as_ref()
            .map(|b| b.state.annotations.clone())
            .unwrap_or_default();

        let modal_open = self.is_modal_open();
        let has_ctx_menu = self.context_menu.is_some();
        let pointer_pos = if modal_open {
            None
        } else {
            ui.ctx().pointer_latest_pos()
        };
        let primary_pressed =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_pressed());
        let primary_down =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_down());
        let primary_released =
            !modal_open && !has_ctx_menu && ui.ctx().input(|i| i.pointer.primary_released());
        let secondary_clicked =
            !modal_open && ui.ctx().input(|i| i.pointer.secondary_clicked());
        let was_dragging = self.flow_sel_dragging;
        if !primary_down {
            self.flow_sel_dragging = false;
        }

        let mut total_h = 88.0f32;
        if let Some(cache) = &self.flow_cache {
            total_h += cache.items.iter().map(|it| it.height).sum::<f32>();
        }

        let scroll_out = scroll.show_viewport(ui, |ui, viewport| {
            let clip = ui.clip_rect();
            let view_pointer_pos = if modal_open || self.flow_width_dragging {
                None
            } else {
                pointer_pos
                    .filter(|&pos| clip.contains(pos) && ui.ctx().layer_id_at(pos) == Some(ui.layer_id()))
            };
            let drag_pointer_pos = if modal_open || self.flow_width_dragging {
                None
            } else {
                pointer_pos.filter(|&pos| clip.contains(pos))
            };
            let hit_pointer_pos = if self.flow_sel_dragging {
                drag_pointer_pos
            } else {
                view_pointer_pos
            };
            let canvas_sense = if modal_open || self.flow_width_dragging {
                Sense::hover()
            } else {
                Sense::click()
            };
            let (canvas_rect, canvas_resp) =
                ui.allocate_exact_size(Vec2::new(avail_w.max(viewport.width()), total_h), canvas_sense);
            let col_left = canvas_rect.left() + ((canvas_rect.width() - measure) * 0.5).max(16.0);
            let vis_top = viewport.min.y;
            let vis_bot = viewport.max.y;

            let Some(cache) = self.flow_cache.as_mut() else {
                return;
            };

            let mut cur_y = 24.0f32;
            let mut first_visible_chapter: Option<usize> = None;
            let sel_range = match (self.flow_sel_anchor, self.flow_sel_head) {
                (Some(a), Some(b)) if a != b => {
                    if a <= b {
                        Some((a, b))
                    } else {
                        Some((b, a))
                    }
                }
                _ => None,
            };

            let mut new_hit_pos: Option<FlowTextPos> = None;
            let mut hover_text_cursor = false;

            for idx in 0..cache.items.len() {
                let item_h = cache.items[idx].height;
                let item_top = cur_y;
                let item_bot = cur_y + item_h;
                cur_y = item_bot;

                if item_bot >= vis_top && item_top <= vis_bot && first_visible_chapter.is_none() {
                    first_visible_chapter = Some(cache.items[idx].chapter_idx);
                }

                if item_bot < vis_top - 450.0 || item_top > vis_bot + 450.0 {
                    continue;
                }

                // Lazily build cached LayoutJob / spec for visible item (never stores stale Galleys)
                if cache.items[idx].visual.is_none() {
                    let mut ctx = FlowDrawCtx {
                        book_id: &book_id,
                        palette: p,
                        mode,
                        font_size,
                        line_height,
                        max_width: measure,
                        resources: &flow.resources,
                        math_cache: &mut self.math_cache,
                        math_textures: &mut self.math_textures,
                        image_textures: &mut self.image_textures,
                        clicked_link: &mut clicked_link,
                        requested_menu: &mut requested_menu,
                    };
                    let (visual, est_h) =
                        build_flow_item_visual(ui, &cache.items[idx].source, &flow, &mut ctx);
                    cache.items[idx].visual = Some(visual);
                    cache.items[idx].height = est_h;
                }

                let actual_h = cache.items[idx].height;
                let item_rect = Rect::from_min_size(
                    Pos2::new(col_left, canvas_rect.top() + item_top),
                    Vec2::new(measure, actual_h),
                );

                let item_sel = sel_range.and_then(|(s, e)| {
                    if idx < s.item_idx || idx > e.item_idx {
                        return None;
                    }
                    let total_c = cache.items[idx].plain.chars().count();
                    let c0 = if idx == s.item_idx {
                        s.char_idx.min(total_c)
                    } else {
                        0
                    };
                    let c1 = if idx == e.item_idx {
                        e.char_idx.min(total_c)
                    } else {
                        total_c
                    };
                    (c1 > c0).then_some((c0, c1))
                });

                let mut updated_h: Option<f32> = None;
                let gap = font_size * 0.55;

                if let Some(visual) = cache.items[idx].visual.as_ref() {
                    match visual {
                        FlowVisual::Separator => {
                            let mid_y = item_rect.center().y;
                            ui.painter().line_segment(
                                [
                                    Pos2::new(item_rect.left(), mid_y),
                                    Pos2::new(item_rect.right(), mid_y),
                                ],
                                Stroke::new(1.0, p.line),
                            );
                        }
                        FlowVisual::SingleJob {
                            job,
                            pad_top,
                            link_spans,
                            code_box,
                            quote_bar,
                        } => {
                            let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
                            let extra_pad = if *code_box {
                                22.0
                            } else if *pad_top > 0.0 {
                                *pad_top + 2.0
                            } else {
                                0.0
                            };
                            updated_h = Some(galley.rect.height() + extra_pad + gap);

                            let content_pos = Pos2::new(
                                item_rect.left()
                                    + if *code_box {
                                        10.0
                                    } else if *quote_bar {
                                        14.0
                                    } else {
                                        0.0
                                    },
                                item_rect.top() + *pad_top,
                            );
                            if *code_box {
                                let box_r = Rect::from_min_size(
                                    Pos2::new(item_rect.left(), item_rect.top() + 2.0),
                                    Vec2::new(
                                        (galley.rect.width() + 20.0).min(measure),
                                        galley.rect.height() + 18.0,
                                    ),
                                );
                                ui.painter().rect_filled(box_r, 8.0, p.code_bg);
                                ui.painter().rect_stroke(
                                    box_r,
                                    8.0,
                                    Stroke::new(1.0, p.line),
                                    StrokeKind::Inside,
                                );
                            } else if *quote_bar {
                                ui.painter().line_segment(
                                    [
                                        Pos2::new(item_rect.left() + 4.0, item_rect.top() + 4.0),
                                        Pos2::new(
                                            item_rect.left() + 4.0,
                                            item_rect.top() + galley.rect.height() + 6.0,
                                        ),
                                    ],
                                    Stroke::new(3.0, p.accent),
                                );
                            }
                            // Paint saved annotations on this flow item
                            let item_plain = &cache.items[idx].plain;
                            let item_ch = cache.items[idx].chapter_idx;
                            if !item_plain.is_empty() && !flow_annotations.is_empty() {
                                for ann in &flow_annotations {
                                    for part in &ann.parts {
                                        if part.index == item_ch
                                            && let Some((hc0, hc1)) =
                                                locate_anchor_in_text(item_plain, part)
                                        {
                                            let hl_col = highlight_fill_color(&ann.color, dark);
                                            let rects = paint_galley_selection_rects(
                                                ui,
                                                &galley,
                                                content_pos,
                                                hc0,
                                                hc1,
                                                hl_col,
                                                ann.kind == "note",
                                                highlight_swatch_color(&ann.color),
                                            );
                                            if primary_released
                                                && (!was_dragging
                                                    || self.flow_sel_anchor == self.flow_sel_head)
                                                && let Some(pos) = view_pointer_pos
                                                && rects.iter().any(|r| r.contains(pos))
                                            {
                                                clicked_annotation_id = Some(ann.id.clone());
                                            }
                                        }
                                    }
                                }
                            }
                            if let Some((c0, c1)) = item_sel {
                                let rects = paint_galley_selection_rects(
                                    ui,
                                    &galley,
                                    content_pos,
                                    c0,
                                    c1,
                                    p.selection,
                                    false,
                                    Color32::TRANSPARENT,
                                );
                                for r in rects {
                                    let vr = r.intersect(clip);
                                    if vr.is_positive() {
                                        sel_bounds = Some(match sel_bounds {
                                            Some(acc) => acc.union(vr),
                                            None => vr,
                                        });
                                    }
                                }
                            }
                            ui.painter().galley(content_pos, galley.clone(), p.ink);

                            if !modal_open {
                                for lb in compute_galley_link_boxes(&galley, link_spans) {
                                    let lr = lb.rel_rect.translate(content_pos.to_vec2()).intersect(clip);
                                    if !lr.is_positive() {
                                        continue;
                                    }
                                    let resp = ui.interact(
                                        lr,
                                        ui.scope_id()
                                            .with(("flow-link", idx, lr.min.x as i32, lr.min.y as i32)),
                                        Sense::click(),
                                    );
                                    if resp.hovered() {
                                        ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                    }
                                    if resp.clicked() {
                                        clicked_link = Some(lb.target);
                                    }
                                }
                            }

                            if let Some(pos) = hit_pointer_pos
                                && item_rect.contains(pos)
                            {
                                if view_pointer_pos.is_some() {
                                    hover_text_cursor = true;
                                }
                                let local = pos - content_pos;
                                let cursor = galley.cursor_from_pos(local);
                                new_hit_pos = Some(FlowTextPos {
                                    item_idx: idx,
                                    char_idx: cursor.index.into(),
                                });
                            }
                        }
                        FlowVisual::CodeLine {
                            line_no_text,
                            code_job,
                            font_size: c_fsize,
                        } => {
                            let line_no_galley = ui.fonts_mut(|f| {
                                f.layout_no_wrap(
                                    line_no_text.clone(),
                                    FontId::monospace(*c_fsize),
                                    p.muted,
                                )
                            });
                            let code_galley = ui.fonts_mut(|f| f.layout_job(code_job.clone()));
                            updated_h = Some((code_galley.rect.height() + 6.0).max(*c_fsize * 1.48));

                            let num_w = 52.0;
                            ui.painter().galley(
                                Pos2::new(item_rect.left(), item_rect.top() + 2.0),
                                line_no_galley,
                                p.muted,
                            );
                            let code_pos =
                                Pos2::new(item_rect.left() + num_w, item_rect.top() + 2.0);
                            if let Some((c0, c1)) = item_sel {
                                let rects = paint_galley_selection_rects(
                                    ui,
                                    &code_galley,
                                    code_pos,
                                    c0,
                                    c1,
                                    p.selection,
                                    false,
                                    Color32::TRANSPARENT,
                                );
                                for r in rects {
                                    let vr = r.intersect(clip);
                                    if vr.is_positive() {
                                        sel_bounds = Some(match sel_bounds {
                                            Some(acc) => acc.union(vr),
                                            None => vr,
                                        });
                                    }
                                }
                            }
                            ui.painter().galley(code_pos, code_galley.clone(), p.ink);
                            if let Some(pos) = hit_pointer_pos
                                && item_rect.contains(pos)
                            {
                                if view_pointer_pos.is_some() {
                                    hover_text_cursor = true;
                                }
                                let cursor = code_galley.cursor_from_pos(pos - code_pos);
                                new_hit_pos = Some(FlowTextPos {
                                    item_idx: idx,
                                    char_idx: cursor.index.into(),
                                });
                            }
                        }
                        FlowVisual::RichSpec { scale_mul, pad_top } => {
                            if let FlowSourceRef::Block {
                                chapter_idx,
                                block_idx,
                            } = &cache.items[idx].source
                            {
                                let inlines = match &flow.chapters[*chapter_idx].blocks[*block_idx] {
                                    Block::Heading { inlines, .. }
                                    | Block::Paragraph(inlines) => Some(inlines.as_slice()),
                                    _ => None,
                                };
                                if let Some(inlines) = inlines {
                                    let mut ctx = FlowDrawCtx {
                                        book_id: &book_id,
                                        palette: p,
                                        mode,
                                        font_size,
                                        line_height,
                                        max_width: measure,
                                        resources: &flow.resources,
                                        math_cache: &mut self.math_cache,
                                        math_textures: &mut self.math_textures,
                                        image_textures: &mut self.image_textures,
                                        clicked_link: &mut clicked_link,
                                        requested_menu: &mut requested_menu,
                                    };
                                    let (lines, _, char_boxes, extra_gap, base_ascent, total_h) =
                                        measure_rich_lines(ui, inlines, *scale_mul, &mut ctx);
                                    updated_h = Some(total_h + *pad_top + gap);
                                    let item_plain = &cache.items[idx].plain;
                                    let item_ch = cache.items[idx].chapter_idx;
                                    let rich_origin =
                                        item_rect.min.to_vec2() + Vec2::new(0.0, *pad_top);
                                    if !item_plain.is_empty() && !flow_annotations.is_empty() {
                                        for ann in &flow_annotations {
                                            for part in &ann.parts {
                                                if part.index == item_ch
                                                    && let Some((hc0, hc1)) =
                                                        locate_anchor_in_text(item_plain, part)
                                                {
                                                    let hl_col =
                                                        highlight_fill_color(&ann.color, dark);
                                                    let ul_col = highlight_swatch_color(&ann.color);
                                                    for r in &char_boxes
                                                        [hc0..hc1.min(char_boxes.len())]
                                                    {
                                                        let sr = r.translate(rich_origin);
                                                        ui.painter().rect_filled(sr, 1.0, hl_col);
                                                        if ann.kind == "note" {
                                                            ui.painter().line_segment(
                                                                [
                                                                    Pos2::new(sr.left(), sr.bottom()),
                                                                    Pos2::new(sr.right(), sr.bottom()),
                                                                ],
                                                                Stroke::new(2.0, ul_col),
                                                            );
                                                        }
                                                        if primary_released
                                                            && (!was_dragging
                                                                || self.flow_sel_anchor
                                                                    == self.flow_sel_head)
                                                            && let Some(pos) = view_pointer_pos
                                                            && sr.contains(pos)
                                                        {
                                                            clicked_annotation_id =
                                                                Some(ann.id.clone());
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    if let Some((c0, c1)) = item_sel {
                                        for r in &char_boxes[c0..c1.min(char_boxes.len())] {
                                            let sr = r.translate(rich_origin);
                                            ui.painter().rect_filled(sr, 1.0, p.selection);
                                            let vr = sr.intersect(clip);
                                            if vr.is_positive() {
                                                sel_bounds = Some(match sel_bounds {
                                                    Some(acc) => acc.union(vr),
                                                    None => vr,
                                                });
                                            }
                                        }
                                    }
                                    let mut click_hit_boxes = Vec::new();
                                    paint_measured_lines(
                                        ui,
                                        &lines,
                                        Pos2::new(item_rect.left(), item_rect.top() + *pad_top),
                                        base_ascent,
                                        extra_gap,
                                        p,
                                        &mut click_hit_boxes,
                                    );
                                    if !modal_open {
                                        for (lr, target) in click_hit_boxes {
                                            let lr = lr.intersect(clip);
                                            if !lr.is_positive() {
                                                continue;
                                            }
                                            let resp = ui.interact(
                                                lr,
                                                ui.scope_id().with((
                                                    "rich-link",
                                                    idx,
                                                    lr.min.x as i32,
                                                    lr.min.y as i32,
                                                )),
                                                Sense::click(),
                                            );
                                            if resp.hovered() {
                                                ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                                            }
                                            if resp.clicked() {
                                                clicked_link = Some(target);
                                            }
                                        }
                                    }
                                    if let Some(pos) = hit_pointer_pos
                                        && item_rect.contains(pos)
                                    {
                                        if view_pointer_pos.is_some() {
                                            hover_text_cursor = true;
                                        }
                                        let origin = item_rect.min + Vec2::new(0.0, *pad_top);
                                        let rel = pos - origin;
                                        let ci = hit_test_char_boxes(rel, &char_boxes);
                                        new_hit_pos = Some(FlowTextPos {
                                            item_idx: idx,
                                            char_idx: ci,
                                        });
                                    }
                                }
                            }
                        }
                        FlowVisual::FallbackBlock => {
                            if let FlowSourceRef::Block {
                                chapter_idx,
                                block_idx,
                            } = &cache.items[idx].source
                            {
                                let block = &flow.chapters[*chapter_idx].blocks[*block_idx];
                                let mut ctx = FlowDrawCtx {
                                    book_id: &book_id,
                                    palette: p,
                                    mode,
                                    font_size,
                                    line_height,
                                    max_width: measure,
                                    resources: &flow.resources,
                                    math_cache: &mut self.math_cache,
                                    math_textures: &mut self.math_textures,
                                    image_textures: &mut self.image_textures,
                                    clicked_link: &mut clicked_link,
                                    requested_menu: &mut requested_menu,
                                };
                                let unclipped = Rect::from_min_size(
                                    item_rect.min,
                                    Vec2::new(measure, 10_000.0),
                                );
                                let scoped =
                                    ui.scope_builder(UiBuilder::new().max_rect(unclipped), |ui| {
                                        draw_flow_block_fallback(ui, block, &mut ctx);
                                    });
                                let block_rect = scoped.response.rect;
                                if let Some((c0, c1)) = item_sel
                                    && c1 > c0
                                {
                                    ui.painter().rect_filled(block_rect, 3.0, p.selection);
                                    let vr = block_rect.intersect(clip);
                                    if vr.is_positive() {
                                        sel_bounds = Some(match sel_bounds {
                                            Some(acc) => acc.union(vr),
                                            None => vr,
                                        });
                                    }
                                }
                                if let Some(pos) = hit_pointer_pos
                                    && block_rect.contains(pos)
                                    && !cache.items[idx].plain.is_empty()
                                {
                                    if view_pointer_pos.is_some() {
                                        hover_text_cursor = true;
                                    }
                                    let total_c = cache.items[idx].plain.chars().count();
                                    let fx = ((pos.x - block_rect.left())
                                        / block_rect.width().max(1.0))
                                    .clamp(0.0, 1.0);
                                    let ci = ((fx * total_c as f32).round() as usize).min(total_c);
                                    new_hit_pos = Some(FlowTextPos {
                                        item_idx: idx,
                                        char_idx: ci,
                                    });
                                }
                                let real_h = (block_rect.height() + gap).max(20.0);
                                updated_h = Some(real_h);
                            }
                        }
                    }
                }
                if let Some(nh) = updated_h
                    && (nh - cache.items[idx].height).abs() > 0.5
                {
                    cache.items[idx].height = nh;
                    ui.ctx().request_repaint();
                }
            }

            if !modal_open && !self.flow_width_dragging {
                if hover_text_cursor {
                    ui.ctx().set_cursor_icon(CursorIcon::Text);
                }
                if primary_pressed && view_pointer_pos.is_some() {
                    if let Some(pos) = new_hit_pos {
                        self.flow_sel_anchor = Some(pos);
                        self.flow_sel_head = Some(pos);
                        self.flow_sel_dragging = true;
                    } else {
                        self.flow_sel_anchor = None;
                        self.flow_sel_head = None;
                    }
                } else if self.flow_sel_dragging && primary_down && let Some(pos) = new_hit_pos {
                    self.flow_sel_head = Some(pos);
                }
                if (canvas_resp.secondary_clicked() || secondary_clicked)
                    && let Some(pointer) = view_pointer_pos
                {
                    let sel = self.selected_flow_text();
                    if !sel.is_empty() {
                        requested_menu = Some(ImageContextMenu {
                            pos: pointer,
                            label: String::new(),
                            filename: String::new(),
                            png_bytes: None,
                            selected_text: Some(sel),
                            extra_text: None,
                        });
                    }
                }
            }

            if let Some(cu) = first_visible_chapter {
                self.current_unit = cu;
            }
            let max_s = (total_h - viewport.height()).max(1.0);
            self.scroll_fraction = (vis_top / max_s).clamp(0.0, 1.0);
        });

        if !modal_open && !is_code_mode {
            let vp = scroll_out.inner_rect;
            let center_x = vp.center().x;
            let col_left_x = (center_x - measure * 0.5).max(vp.left() + 8.0);
            let col_right_x = (center_x + measure * 0.5).min(vp.right() - 22.0);
            let left_zone = Rect::from_min_max(
                Pos2::new((col_left_x - 18.0).max(vp.left()), vp.top()),
                Pos2::new((col_left_x - 2.0).max(vp.left() + 10.0), vp.bottom()),
            );
            let right_zone = Rect::from_min_max(
                Pos2::new((col_right_x + 2.0).min(vp.right() - 24.0), vp.top()),
                Pos2::new((col_right_x + 18.0).min(vp.right() - 14.0), vp.bottom()),
            );
            let mut active_drag = false;
            for (salt, zone, guide_x) in [
                ("flow_width_drag_left", left_zone, col_left_x),
                ("flow_width_drag_right", right_zone, col_right_x),
            ] {
                if !zone.is_positive() {
                    continue;
                }
                let resp = ui
                    .interact(zone, ui.scope_id().with(salt), Sense::drag())
                    .on_hover_text("拖动调整阅读区域宽度");
                let dragging = resp.dragged();
                if resp.hovered() || dragging {
                    ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
                    let stroke = if dragging {
                        Stroke::new(1.5, p.accent)
                    } else {
                        Stroke::new(1.0, p.line)
                    };
                    ui.painter().line_segment(
                        [Pos2::new(guide_x, vp.top()), Pos2::new(guide_x, vp.bottom())],
                        stroke,
                    );
                    let grip = Rect::from_center_size(
                        Pos2::new(guide_x, vp.center().y),
                        Vec2::new(4.0, 36.0),
                    );
                    ui.painter().rect_filled(
                        grip,
                        2.0,
                        if dragging { p.accent } else { p.muted },
                    );
                }
                if dragging {
                    active_drag = true;
                    self.flow_sel_dragging = false;
                    if let Some(pos) = ui.ctx().pointer_latest_pos() {
                        let new_w = ((pos.x - center_x).abs() * 2.0).clamp(min_measure, max_measure);
                        self.settings.text_width = new_w.round();
                    }
                }
                if resp.drag_stopped() {
                    self.flow_width_dragging = false;
                    self.save_settings();
                }
            }
            self.flow_width_dragging = active_drag;
        } else {
            self.flow_width_dragging = false;
        }

        self.last_scroll_y = scroll_out.state.clamped_offset().y;
        self.last_viewport_h = scroll_out.inner_rect.height();
        self.last_content_h = scroll_out.content_size.y;
        self.last_selection_rect = sel_bounds;

        if let Some(ann_id) = clicked_annotation_id
            && let Some(ann) = self
                .active_book
                .as_ref()
                .and_then(|b| b.state.annotations.iter().find(|a| a.id == ann_id))
                .cloned()
        {
            self.open_annotation_editor_for(&ann);
        }
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
            let mut add_hl_color: Option<&'static str> = None;
            let mut open_note = false;
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
                            ui.set_min_width(156.0);
                            if let Some(sel) = &menu.selected_text {
                                if ui.button("复制选中文字").clicked() {
                                    copy_text = Some(sel.clone());
                                    keep = false;
                                }
                                if ui.button("高亮选中文字").clicked() {
                                    add_hl_color = Some("yellow");
                                    keep = false;
                                }
                                if ui.button("为选中文字添加笔记…").clicked() {
                                    open_note = true;
                                    keep = false;
                                }
                            }
                            if let Some(bytes) = &menu.png_bytes {
                                if ui.button(&menu.label).clicked() {
                                    copy_bytes = Some(bytes.clone());
                                    keep = false;
                                }
                                if ui.button("图像另存为 PNG…").clicked() {
                                    save_req = Some((bytes.clone(), menu.filename.clone()));
                                    keep = false;
                                }
                            }
                            if let Some(txt) = &menu.extra_text
                                && ui.button("复制本页文字").clicked()
                            {
                                copy_text = Some(txt.clone());
                                keep = false;
                            }
                            if ui.button("取消").clicked() {
                                keep = false;
                            }
                        })
                        .response;
                    let clicked_outside = ctx.input(|i| {
                        i.pointer.primary_pressed()
                            && i.pointer
                                .latest_pos()
                                .is_none_or(|pos| !frame_resp.rect.contains(pos))
                    });
                    if clicked_outside {
                        keep = false;
                    }
                });
            if let Some(txt) = copy_text {
                self.copy_text_reliable(ctx, txt, "已复制文字到剪贴板。");
            }
            if let Some(col) = add_hl_color {
                self.add_highlight_from_selection(col);
            }
            if open_note {
                self.open_note_editor_from_selection();
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

        // Floating selection toolbar (Copy, 4-color highlights, Note)
        if !self.is_modal_open()
            && self.context_menu.is_none()
            && !self.flow_sel_dragging
            && !self.fixed_sel_dragging
            && let Some(sel_rect) = self.last_selection_rect
        {
            let sel_text = if self
                .active_book
                .as_ref()
                .is_some_and(|b| matches!(b.source, LoadedSource::Flow(_)))
            {
                self.selected_flow_text()
            } else {
                self.selected_fixed_text()
            };
            if !sel_text.trim().is_empty() {
                let screen = ctx.content_rect();
                let bar_w = 248.0;
                let bar_h = 36.0;
                let x = (sel_rect.center().x - bar_w * 0.5)
                    .clamp(screen.left() + 8.0, (screen.right() - bar_w - 8.0).max(8.0));
                let y = if sel_rect.top() - bar_h - 8.0 >= screen.top() + 54.0 {
                    sel_rect.top() - bar_h - 8.0
                } else {
                    (sel_rect.bottom() + 8.0).min(screen.bottom() - bar_h - 46.0)
                };
                let mut do_copy = false;
                let mut hl_color: Option<&'static str> = None;
                let mut do_note = false;
                egui::Area::new(Id::unique("selection_toolbar_area"))
                    .fixed_pos(Pos2::new(x, y))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        Frame::new()
                            .fill(p.surface)
                            .stroke(Stroke::new(1.0, p.line))
                            .corner_radius(CornerRadius::same(9))
                            .inner_margin(Margin::symmetric(6, 4))
                            .shadow(egui::Shadow {
                                offset: [0, 4],
                                blur: 12,
                                spread: 0,
                                color: Color32::from_black_alpha(48),
                            })
                            .show(ui, |ui| {
                                ui.horizontal(|ui| {
                                    ui.spacing_mut().item_spacing.x = 5.0;
                                    if ui.button("复制").clicked() {
                                        do_copy = true;
                                    }
                                    for (name, tip) in [
                                        ("yellow", "黄色高亮"),
                                        ("green", "绿色高亮"),
                                        ("blue", "蓝色高亮"),
                                        ("pink", "粉色高亮"),
                                    ] {
                                        let (r, resp) = ui
                                            .allocate_exact_size(Vec2::splat(22.0), Sense::click());
                                        let resp = resp.on_hover_text(tip);
                                        let fill = highlight_swatch_color(name);
                                        ui.painter().circle_filled(r.center(), 9.0, fill);
                                        ui.painter().circle_stroke(
                                            r.center(),
                                            9.0,
                                            Stroke::new(
                                                if resp.hovered() { 1.8 } else { 1.0 },
                                                if resp.hovered() { p.accent } else { Color32::from_black_alpha(55) },
                                            ),
                                        );
                                        if resp.clicked() {
                                            hl_color = Some(name);
                                        }
                                    }
                                    if ui.button("笔记").clicked() {
                                        do_note = true;
                                    }
                                });
                            });
                    });
                if do_copy {
                    self.copy_text_reliable(ctx, sel_text, "已复制选中文字。");
                } else if let Some(col) = hl_color {
                    self.add_highlight_from_selection(col);
                } else if do_note {
                    self.open_note_editor_from_selection();
                }
            }
        }

        let modal_frame = Frame::new()
            .fill(p.surface)
            .stroke(Stroke::new(1.0, p.line))
            .corner_radius(CornerRadius::same(10))
            .inner_margin(Margin::same(16));

        if let Some(modal) = self.link_warning.take() {
            let mut keep = true;
            egui::Modal::new(Id::unique("link_warning_modal"))
                .frame(modal_frame)
                .show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.heading("外部链接风险提示");
                    ui.separator();
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
                            self.copy_text_reliable(ctx, modal.target.clone(), "已复制链接。");
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

        if let Some(mut editor) = self.annotation_editor.take() {
            let mut keep = true;
            let mut do_save = false;
            let mut do_delete = false;
            egui::Modal::new(Id::unique("annotation_editor_modal"))
                .frame(modal_frame)
                .show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    let heading = match (editor.existing_id.is_some(), editor.kind.as_str()) {
                        (true, "bookmark") => "编辑书签",
                        (true, "highlight") => "编辑高亮",
                        (true, _) => "编辑笔记",
                        (false, _) => "添加笔记",
                    };
                    ui.heading(heading);
                    ui.separator();
                    let quote_text: String = editor
                        .parts
                        .iter()
                        .map(|pt| pt.quote.trim())
                        .filter(|s| !s.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n");
                    if !quote_text.is_empty() {
                        Frame::new()
                            .fill(p.side)
                            .stroke(Stroke::new(1.0, p.line))
                            .corner_radius(CornerRadius::same(6))
                            .inner_margin(Margin::same(8))
                            .show(ui, |ui| {
                                ui.set_width(ui.available_width());
                                ui.label(
                                    RichText::new(truncate_str(&quote_text, 240)).color(p.muted),
                                );
                            });
                        ui.add_space(8.0);
                    }
                    if editor.kind != "bookmark" {
                        ui.horizontal(|ui| {
                            ui.label("高亮颜色：");
                            for (key, label) in [
                                ("yellow", "黄"),
                                ("green", "绿"),
                                ("blue", "蓝"),
                                ("pink", "粉"),
                            ] {
                                let selected = editor.color == key;
                                let (r, resp) =
                                    ui.allocate_exact_size(Vec2::new(42.0, 24.0), Sense::click());
                                let fill = highlight_swatch_color(key);
                                ui.painter().rect_filled(
                                    r,
                                    6.0,
                                    Color32::from_rgba_unmultiplied(
                                        fill.r(),
                                        fill.g(),
                                        fill.b(),
                                        if selected { 170 } else { 75 },
                                    ),
                                );
                                ui.painter().rect_stroke(
                                    r,
                                    6.0,
                                    Stroke::new(
                                        if selected { 1.8 } else { 1.0 },
                                        if selected { p.accent } else { p.line },
                                    ),
                                    StrokeKind::Inside,
                                );
                                ui.painter().text(
                                    r.center(),
                                    egui::Align2::CENTER_CENTER,
                                    label,
                                    FontId::proportional(13.0),
                                    p.ink,
                                );
                                if resp.clicked() {
                                    editor.color = key.to_owned();
                                }
                            }
                        });
                        ui.add_space(6.0);
                    }
                    let note_label = if editor.kind == "bookmark" {
                        "书签名称"
                    } else {
                        "笔记内容（留空则仅保留高亮）"
                    };
                    ui.label(note_label);
                    ui.add(
                        egui::TextEdit::multiline(&mut editor.text)
                            .desired_rows(4)
                            .desired_width(f32::INFINITY),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if editor.existing_id.is_some()
                            && ui
                                .button(RichText::new("删除").color(Color32::from_rgb(0xb5, 0x47, 0x35)))
                                .clicked()
                        {
                            do_delete = true;
                            keep = false;
                        }
                        ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                            if ui
                                .add(
                                    egui::Button::new(RichText::new("保存").color(p.accent_ink))
                                        .fill(p.accent),
                                )
                                .clicked()
                            {
                                do_save = true;
                                keep = false;
                            }
                            if ui.button("取消").clicked() {
                                keep = false;
                            }
                        });
                    });
                });
            if do_delete
                && let Some(eid) = &editor.existing_id
                && let Some(book) = self.active_book.as_mut()
            {
                book.state.annotations.retain(|a| a.id != *eid);
                self.save_active_book();
                self.sync_auto_sidebar(false);
                self.set_status("已删除批注。", Some(2));
            } else if do_save
                && let Some(book) = self.active_book.as_mut()
            {
                let trimmed = editor.text.trim().to_owned();
                let final_kind = if editor.kind == "bookmark" {
                    "bookmark".to_owned()
                } else if trimmed.is_empty() && !editor.parts.is_empty() {
                    "highlight".to_owned()
                } else {
                    "note".to_owned()
                };
                if final_kind == "note" && trimmed.is_empty() && editor.parts.is_empty() {
                    self.set_status("笔记内容不能为空。", Some(2));
                } else {
                    if let Some(eid) = &editor.existing_id {
                        if let Some(existing) =
                            book.state.annotations.iter_mut().find(|a| a.id == *eid)
                        {
                            existing.kind = final_kind;
                            existing.color = editor.color.clone();
                            existing.text = if trimmed.is_empty() {
                                existing
                                    .parts
                                    .first()
                                    .map(|pt| truncate_str(pt.quote.trim(), 60))
                                    .unwrap_or_default()
                            } else {
                                trimmed
                            };
                        }
                    } else {
                        let digest = book.digest.clone();
                        book.state.annotations.push(Annotation {
                            id: uuid::Uuid::new_v4().to_string(),
                            kind: final_kind,
                            color: editor.color.clone(),
                            text: trimmed,
                            parts: editor.parts.clone(),
                            position: editor.position.clone(),
                            digest,
                            missing: false,
                        });
                    }
                    self.sidebar_open = true;
                    self.sidebar_tab = SidebarTab::Notes;
                    self.save_active_book();
                    self.clear_selection();
                    self.set_status("批注已保存。", Some(2));
                }
            }
            if keep {
                self.annotation_editor = Some(editor);
            }
        }

        if self.smart_invert_hint_open {
            if !self.active_book_uses_smart_invert() || !self.is_smart_invert() {
                self.smart_invert_hint_open = false;
            } else if let Some(anchor) = self.smart_invert_btn_rect {
                let dismissed = draw_callout_bubble(
                    ctx,
                    "smart_invert_hint_bubble",
                    anchor,
                    true,
                    "智能反色已开启",
                    "深色模式下已为 PDF / Word 自动启用智能反色（保留鲜亮色彩与插图，扫描 PDF 整页反色）。点击下方状态栏的「已启用智能反色」按钮可随时切换关闭。",
                    p,
                );
                if dismissed {
                    self.settings.smart_invert_hint_seen = true;
                    self.smart_invert_hint_open = false;
                    self.save_settings();
                }
            } else {
                ctx.request_repaint();
            }
        }

        if self.tab_sleep_hint_open
            && !self.smart_invert_hint_open
            && !self.show_install_dialog
            && !self.show_update_dialog
            && self.link_warning.is_none()
            && self.annotation_editor.is_none()
        {
            let target_rect = self
                .tab_sleep_hint_tab_id
                .as_ref()
                .and_then(|id| self.tab_screen_rects.get(id).copied())
                .or_else(|| {
                    self.tabs
                        .iter()
                        .find(|t| self.is_tab_sleeping(&t.id))
                        .and_then(|t| self.tab_screen_rects.get(&t.id).copied())
                })
                .unwrap_or_else(|| {
                    let s = ctx.content_rect();
                    Rect::from_center_size(Pos2::new(s.center().x, s.top() + 64.0), Vec2::new(120.0, 24.0))
                });
            let dismissed = draw_callout_bubble(
                ctx,
                "tab_sleep_hint_bubble",
                target_rect,
                false,
                "标签页已进入节能休眠",
                "后台标签页闲置 5 分钟或超出最近 5 个时会自动进入节能休眠以释放内存（文字变灰并显示绿色节能图标）。点击该标签即可随时恢复阅读进度。",
                p,
            );
            if dismissed {
                self.settings.tab_sleep_hint_seen = true;
                self.tab_sleep_hint_open = false;
                self.tab_sleep_hint_tab_id = None;
                self.save_settings();
            }
        }

        if self.show_settings {
            let mut close = false;
            let mut changed = false;
            let mut open_install = false;
            let mut check_upd = false;
            egui::Window::new("设置")
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
                    ui.scope(|ui| {
                        let vis = ui.visuals_mut();
                        vis.slider_trailing_fill = true;
                        vis.selection.bg_fill = p.accent;
                        changed |= ui
                            .add(
                                egui::Slider::new(&mut self.settings.font_size, 13.0..=34.0)
                                    .text("字号"),
                            )
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
                                egui::Slider::new(&mut self.settings.text_width, 360.0..=1800.0)
                                    .step_by(10.0)
                                    .text("阅读宽度"),
                            )
                            .changed();
                    });
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
                            let seen_si = self.settings.smart_invert_hint_seen;
                            let seen_sleep = self.settings.tab_sleep_hint_seen;
                            self.settings = Settings {
                                smart_invert_hint_seen: seen_si,
                                tab_sleep_hint_seen: seen_sleep,
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
                self.flow_cache = None;
                self.save_settings();
                self.maybe_trigger_smart_invert_hint();
            }
            if open_install {
                self.show_install_dialog = true;
            }
            if check_upd {
                self.spawn_release_check(ctx, true);
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
                    fragment: String::new(),
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
            egui::Modal::new(Id::unique("install_modal"))
                .frame(modal_frame)
                .show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.heading(if st.installed {
                        "文件关联"
                    } else {
                        "安装 HayakuReader"
                    });
                    ui.separator();
                    ui.label(st.path.display().to_string());
                    ui.separator();
                    ui.horizontal_wrapped(|ui| {
                        for ext in EXTENSIONS {
                            let checked =
                                self.install_selected.entry((*ext).to_owned()).or_insert(false);
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
                    ui.add_space(8.0);
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
                        self.release_instance_lock();
                        if let Err(e) = platform::restart(restart_req) {
                            self.set_status(format!("重启失败：{e}"), None);
                            self.show_install_dialog = false;
                        } else {
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
                self.maybe_trigger_smart_invert_hint();
            }
        }

        if self.show_update_dialog
            && let Some(rel) = self.pending_release.clone()
        {
            let mut close = false;
            egui::Modal::new(Id::unique("update_modal"))
                .frame(modal_frame)
                .show(ctx, |ui| {
                    ui.set_max_width(440.0);
                    ui.heading(format!("发现新版本 {}", rel.tag_name));
                    ui.separator();
                    if !platform::installed() {
                        ui.label("当前为便携运行版。如需应用内更新，请先通过「设置 -> 安装」安装到本机，或前往 GitHub 手动下载。");
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
                                let repaint_ctx = ctx.clone();
                                std::thread::spawn(move || {
                                    let res = platform::download_and_stage_update(&data, &rel, &slot);
                                    let _ = tx.send(BgEvent::UpdateFinished(res));
                                    repaint_ctx.request_repaint();
                                });
                            }
                        });
                    }
                });
            if close {
                self.show_update_dialog = false;
                self.maybe_trigger_smart_invert_hint();
            }
        }
    }

    fn draw_splash(&mut self, ui: &mut Ui, subtitle: Option<&str>) -> bool {
        let p = self.palette();
        let mut skipped = false;
        CentralPanel::default()
            .frame(Frame::new().fill(p.paper))
            .show(ui, |ui| {
                let full_rect = ui.max_rect();
                let resp = ui.interact(full_rect, Id::unique("splash_bg"), Sense::click());
                if resp.clicked() || ui.ctx().input(|i| i.key_pressed(Key::Escape)) {
                    skipped = true;
                }
                ui.scope_builder(UiBuilder::new().max_rect(full_rect), |ui| {
                    ui.vertical_centered(|ui| {
                        let content_h = 190.0;
                        let top_pad = ((full_rect.height() - content_h) * 0.5).max(24.0);
                        ui.add_space(top_pad);
                        if let Some(logo) = &self.logo_texture {
                            ui.add(egui::Image::new((logo.id(), Vec2::splat(80.0))));
                            ui.add_space(14.0);
                        }
                        ui.label(
                            RichText::new("HayakuReader")
                                .size(36.0)
                                .strong()
                                .color(p.ink),
                        );
                        ui.add_space(6.0);
                        ui.label(RichText::new("drliuhuan").size(15.0).color(p.muted));
                        ui.add_space(3.0);
                        ui.label(
                            RichText::new("github.com/drliuhuan/HayakuReader")
                                .size(14.0)
                                .color(p.muted),
                        );
                        if let Some(sub) = subtitle {
                            ui.add_space(14.0);
                            ui.label(RichText::new(sub).size(13.5).color(p.accent));
                        }
                    });
                });
            });
        skipped
    }

    fn step_splash(&mut self, ui: &mut Ui) -> bool {
        let ctx = ui.ctx().clone();
        match std::mem::replace(&mut self.splash, SplashState::Done) {
            SplashState::Done => false,
            SplashState::LoadingFile {
                primary,
                extra_sleeping,
                frames_rendered,
            } => {
                if frames_rendered == 0 {
                    let fname = primary
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| "文档".to_owned());
                    let msg = format!("正在打开 {fname}…");
                    self.draw_splash(ui, Some(&msg));
                    self.splash = SplashState::LoadingFile {
                        primary,
                        extra_sleeping,
                        frames_rendered: 1,
                    };
                    ctx.request_repaint();
                    true
                } else {
                    for extra in extra_sleeping {
                        self.register_sleeping_path(&extra);
                    }
                    self.open_path(&primary);
                    self.splash = SplashState::Done;
                    self.on_splash_finished();
                    ctx.request_repaint();
                    false
                }
            }
            SplashState::Timed {
                started_at,
                duration,
                frames_rendered,
                mut restored,
            } => {
                if frames_rendered >= 1 && !restored {
                    self.restore_active_session_tab_only();
                    restored = true;
                }
                let elapsed = started_at.elapsed();
                if elapsed >= duration {
                    if !restored {
                        self.restore_active_session_tab_only();
                    }
                    self.splash = SplashState::Done;
                    self.on_splash_finished();
                    false
                } else {
                    let skipped = self.draw_splash(ui, None);
                    if skipped {
                        if !restored {
                            self.restore_active_session_tab_only();
                        }
                        self.splash = SplashState::Done;
                        self.on_splash_finished();
                        false
                    } else {
                        self.splash = SplashState::Timed {
                            started_at,
                            duration,
                            frames_rendered: frames_rendered.saturating_add(1),
                            restored,
                        };
                        let remaining = duration.saturating_sub(elapsed);
                        ctx.request_repaint_after(remaining.min(Duration::from_millis(50)));
                        true
                    }
                }
            }
        }
    }
}

impl eframe::App for HayakuApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll_background(&ctx);
        self.apply_egui_visuals(&ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            self.save_active_book();
            self.save_workspace();
        }

        let p = self.palette();
        if self.step_splash(ui) {
            if !ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
                ctx.layer_painter(egui::LayerId::new(
                    egui::Order::Foreground,
                    Id::unique("window_outer_border"),
                ))
                .rect_stroke(
                    ctx.content_rect(),
                    0.0,
                    Stroke::new(1.0, p.line),
                    StrokeKind::Inside,
                );
            }
            return;
        }

        // Periodically check background tabs for 5-minute idle sleep
        if !self.background_books.is_empty() {
            if self.last_sleep_check.elapsed() >= Duration::from_secs(1) {
                self.last_sleep_check = Instant::now();
                self.sleep_inactive_tabs_at(now_epoch_ms());
            }
            ctx.request_repaint_after(Duration::from_secs(5));
        }

        let mut dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .map(|f| f.path().to_path_buf())
                .collect()
        });
        if let Some(primary) = dropped.pop() {
            for extra in dropped {
                self.register_sleeping_path(&extra);
            }
            self.open_path(&primary);
        }

        self.handle_shortcuts(&ctx);

        if !self.focus_mode {
            self.draw_top_bar(ui);
            self.draw_tabs_bar(ui);
        }
        self.draw_bottom_bar(ui);
        self.draw_sidebar(ui);

        CentralPanel::default()
            .frame(Frame::new().fill(p.paper))
            .show(ui, |ui| {
                match self.active_book.as_ref().map(|b| &b.source) {
                    None => {
                        self.last_selection_rect = None;
                        self.draw_welcome(ui);
                    }
                    Some(LoadedSource::Pdf(_) | LoadedSource::Word(_)) => {
                        self.draw_fixed_viewer(ui)
                    }
                    Some(LoadedSource::Flow(_)) => self.draw_flow_viewer(ui),
                }
            });

        self.draw_popups(&ctx);
        if !ctx.input(|i| i.viewport().maximized.unwrap_or(false)) {
            ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                Id::unique("window_outer_border"),
            ))
            .rect_stroke(
                ctx.content_rect(),
                0.0,
                Stroke::new(1.0, p.line),
                StrokeKind::Inside,
            );
        }
        self.draw_resize_edges(&ctx);
    }

    fn on_exit(&mut self) {
        self.bg_stop.store(true, Ordering::Relaxed);
        self.save_active_book();
        self.save_workspace();
        self.release_instance_lock();
    }
}

fn drain_inbox_dir(inbox: &Path) -> Option<Vec<PathBuf>> {
    let entries = fs::read_dir(inbox).ok()?;
    let mut processed = false;
    let mut collected = Vec::new();
    for file in entries
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "json"))
    {
        let path = file.path();
        if let Ok(paths) = storage::read_json::<Vec<PathBuf>>(&path) {
            let _ = fs::remove_file(&path);
            collected.extend(paths);
            processed = true;
        }
    }
    processed.then_some(collected)
}

struct FlowDrawCtx<'a> {
    book_id: &'a str,
    palette: Palette,
    mode: ThemeMode,
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
        text: String,
        link: Option<LinkTarget>,
        code_bg: bool,
    },
    Ruby {
        base: Arc<egui::Galley>,
        rt: Arc<egui::Galley>,
        base_str: String,
    },
    Math {
        tex: egui::TextureId,
        size: Vec2,
        baseline_from_top: f32,
        latex: String,
    },
}

struct MeasuredLine {
    items: Vec<LineItem>,
    ascent: f32,
    descent: f32,
}

enum FlatToken {
    Text {
        text: String,
        italic: bool,
        strike: bool,
        code: bool,
        superscript: bool,
        subscript: bool,
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

fn contains_ruby_or_math(inlines: &[Inline]) -> bool {
    inlines.iter().any(|inl| match inl {
        Inline::Ruby { .. } | Inline::InlineMath { .. } => true,
        Inline::Styled {
            superscript,
            subscript,
            inlines,
            ..
        } => *superscript || *subscript || contains_ruby_or_math(inlines),
        Inline::Link { inlines, .. } => contains_ruby_or_math(inlines),
        _ => false,
    })
}

fn inlines_plain_text(inlines: &[Inline], out: &mut String) {
    for inl in inlines {
        match inl {
            Inline::Text(t) => out.push_str(t),
            Inline::Styled { inlines, .. } | Inline::Link { inlines, .. } => {
                inlines_plain_text(inlines, out);
            }
            Inline::Ruby { base, .. } => out.push_str(base),
            Inline::InlineMath { latex } => out.push_str(latex),
            Inline::LineBreak => out.push('\n'),
        }
    }
}

fn block_plain_text(block: &Block) -> String {
    let mut s = String::new();
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            inlines_plain_text(inlines, &mut s);
        }
        Block::Code { code, .. } => s.push_str(code),
        Block::DisplayMath { latex } => s.push_str(latex),
        Block::Quote(inner) => {
            for b in inner {
                let sub = block_plain_text(b);
                if !sub.is_empty() {
                    if !s.is_empty() {
                        s.push('\n');
                    }
                    s.push_str(&sub);
                }
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for b in item {
                    let sub = block_plain_text(b);
                    if !sub.is_empty() {
                        if !s.is_empty() {
                            s.push('\n');
                        }
                        s.push_str(&sub);
                    }
                }
            }
        }
        Block::Table { headers, rows } => {
            for cell in headers {
                inlines_plain_text(cell, &mut s);
                s.push('\t');
            }
            for row in rows {
                if !s.is_empty() {
                    s.push('\n');
                }
                for cell in row {
                    inlines_plain_text(cell, &mut s);
                    s.push('\t');
                }
            }
        }
        Block::Image { alt, .. } => s.push_str(alt),
        Block::Rule => {}
    }
    s
}

fn estimate_block_height(
    block: &Block,
    plain: &str,
    chars_per_line: f32,
    line_px: f32,
    font_size: f32,
) -> f32 {
    let gap = font_size * 0.55;
    match block {
        Block::Heading { level, .. } => {
            let mul = match level {
                1 => 1.65,
                2 => 1.38,
                3 => 1.2,
                _ => 1.08,
            };
            let lines = ((plain.chars().count() as f32) / (chars_per_line / mul)).ceil().max(1.0);
            lines * line_px * mul + 8.0 + gap
        }
        Block::Paragraph(_) => {
            let lines = ((plain.chars().count() as f32) / chars_per_line).ceil().max(1.0);
            lines * line_px + gap
        }
        Block::Code { code, .. } => {
            let lines = code.lines().count().max(1) as f32;
            lines * (font_size * 0.85 * 1.4) + 22.0 + gap
        }
        Block::DisplayMath { .. } => font_size * 2.8 + 14.0 + gap,
        Block::Image { .. } => 260.0 + gap,
        Block::Rule => 14.0 + gap,
        Block::Quote(inner) => {
            let sum: f32 = inner
                .iter()
                .map(|b| estimate_block_height(b, &block_plain_text(b), chars_per_line, line_px, font_size))
                .sum();
            sum + 12.0 + gap
        }
        Block::List { items, .. } => {
            let mut sum = gap;
            for item in items {
                for b in item {
                    sum += estimate_block_height(b, &block_plain_text(b), chars_per_line, line_px, font_size);
                }
            }
            sum
        }
        Block::Table { rows, .. } => (rows.len().max(1) as f32 + 1.0) * (line_px + 6.0) + 16.0 + gap,
    }
}

fn build_single_job_inlines(
    inlines: &[Inline],
    fsize: f32,
    palette: Palette,
    max_w: f32,
) -> (LayoutJob, Vec<(usize, usize, LinkTarget)>) {
    let mut job = LayoutJob::default();
    job.wrap.max_width = max_w;
    let mut plain = String::new();
    let mut link_spans: Vec<(usize, usize, LinkTarget)> = Vec::new();
    append_job_inlines(
        inlines,
        false,
        false,
        false,
        None,
        fsize,
        palette,
        &mut job,
        &mut plain,
        &mut link_spans,
    );
    (job, link_spans)
}

fn compute_galley_link_boxes(
    galley: &egui::Galley,
    link_spans: &[(usize, usize, LinkTarget)],
) -> Vec<HitLinkBox> {
    let mut hit_links = Vec::new();
    for (c0, c1, target) in link_spans {
        if *c1 > *c0 {
            let mut offset = 0usize;
            for row in &galley.rows {
                let len = row.row.glyphs.len();
                let end = offset + len + usize::from(row.ends_with_newline);
                if *c1 > offset && *c0 < offset + len {
                    let s0 = c0.saturating_sub(offset).min(len);
                    let s1 = c1.saturating_sub(offset).min(len);
                    if s1 > s0 {
                        let x0 = row.pos.x + row.row.glyphs[s0].pos.x;
                        let last = &row.row.glyphs[s1 - 1];
                        let x1 = row.pos.x + last.pos.x + last.advance_width;
                        let r = Rect::from_min_max(
                            Pos2::new(x0, row.pos.y),
                            Pos2::new(x1, row.pos.y + row.rect().height()),
                        );
                        hit_links.push(HitLinkBox {
                            rel_rect: r,
                            target: target.clone(),
                        });
                    }
                }
                offset = end;
            }
        }
    }
    hit_links
}

#[allow(clippy::too_many_arguments)]
fn append_job_inlines(
    inlines: &[Inline],
    italic: bool,
    strike: bool,
    code: bool,
    link: Option<LinkTarget>,
    fsize: f32,
    palette: Palette,
    job: &mut LayoutJob,
    plain: &mut String,
    link_spans: &mut Vec<(usize, usize, LinkTarget)>,
) {
    for inl in inlines {
        match inl {
            Inline::Text(text) => {
                let start = plain.chars().count();
                plain.push_str(text);
                let end = plain.chars().count();
                let color = if link.is_some() { palette.accent } else { palette.ink };
                let fid = if code {
                    FontId::monospace(fsize * 0.92)
                } else {
                    FontId::proportional(fsize)
                };
                job.append(
                    text,
                    0.0,
                    egui::TextFormat {
                        font_id: fid,
                        color,
                        background: if code { palette.code_bg } else { Color32::TRANSPARENT },
                        italics: italic,
                        underline: if link.is_some() {
                            Stroke::new(1.0, palette.accent)
                        } else {
                            Stroke::NONE
                        },
                        strikethrough: if strike { Stroke::new(1.0, color) } else { Stroke::NONE },
                        ..Default::default()
                    },
                );
                if let Some(t) = &link {
                    link_spans.push((start, end, t.clone()));
                }
            }
            Inline::LineBreak => {
                plain.push('\n');
                job.append(
                    "\n",
                    0.0,
                    egui::TextFormat {
                        font_id: FontId::proportional(fsize),
                        color: palette.ink,
                        ..Default::default()
                    },
                );
            }
            Inline::Styled {
                italic: it,
                strike: st,
                code: cd,
                inlines,
                ..
            } => append_job_inlines(
                inlines,
                italic || *it,
                strike || *st,
                code || *cd,
                link.clone(),
                fsize,
                palette,
                job,
                plain,
                link_spans,
            ),
            Inline::Link { target, inlines } => append_job_inlines(
                inlines,
                italic,
                strike,
                code,
                Some(target.clone()),
                fsize,
                palette,
                job,
                plain,
                link_spans,
            ),
            Inline::Ruby { .. } | Inline::InlineMath { .. } => {}
        }
    }
}

fn build_flow_item_visual(
    ui: &mut Ui,
    source: &FlowSourceRef,
    flow: &FlowDocument,
    ctx: &mut FlowDrawCtx<'_>,
) -> (FlowVisual, f32) {
    let gap = ctx.font_size * 0.55;
    match source {
        FlowSourceRef::Separator => (FlowVisual::Separator, 36.0),
        FlowSourceRef::CodeLine {
            line_no,
            text,
            lang,
        } => {
            let fsize = (ctx.font_size * 0.88).max(12.0);
            let mut code_job = highlight::highlight_line(text, *lang, ctx.mode, fsize);
            code_job.wrap.max_width = (ctx.max_width - 56.0).max(120.0);
            let galley = ui.fonts_mut(|f| f.layout_job(code_job.clone()));
            let h = (galley.rect.height() + 6.0).max(fsize * 1.48);
            (
                FlowVisual::CodeLine {
                    line_no_text: format!("{line_no:>5}"),
                    code_job,
                    font_size: fsize,
                },
                h,
            )
        }
        FlowSourceRef::Block {
            chapter_idx,
            block_idx,
        } => {
            let block = &flow.chapters[*chapter_idx].blocks[*block_idx];
            match block {
                Block::Heading { level, inlines, .. } => {
                    let mul = match level {
                        1 => 1.65,
                        2 => 1.38,
                        3 => 1.2,
                        _ => 1.08,
                    };
                    if contains_ruby_or_math(inlines) {
                        let (_, _, _, _, _, total_h) =
                            measure_rich_lines(ui, inlines, mul, ctx);
                        (
                            FlowVisual::RichSpec {
                                scale_mul: mul,
                                pad_top: 6.0,
                            },
                            total_h + 6.0 + gap,
                        )
                    } else {
                        let (job, link_spans) = build_single_job_inlines(
                            inlines,
                            ctx.font_size * mul,
                            ctx.palette,
                            ctx.max_width,
                        );
                        let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
                        let h = galley.rect.height() + 8.0 + gap;
                        (
                            FlowVisual::SingleJob {
                                job,
                                pad_top: 6.0,
                                link_spans,
                                code_box: false,
                                quote_bar: false,
                            },
                            h,
                        )
                    }
                }
                Block::Paragraph(inlines) => {
                    if contains_ruby_or_math(inlines) {
                        let (_, _, _, _, _, total_h) =
                            measure_rich_lines(ui, inlines, 1.0, ctx);
                        (
                            FlowVisual::RichSpec {
                                scale_mul: 1.0,
                                pad_top: 0.0,
                            },
                            total_h + gap,
                        )
                    } else {
                        let (job, link_spans) = build_single_job_inlines(
                            inlines,
                            ctx.font_size,
                            ctx.palette,
                            ctx.max_width,
                        );
                        let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
                        let h = galley.rect.height() + gap;
                        (
                            FlowVisual::SingleJob {
                                job,
                                pad_top: 0.0,
                                link_spans,
                                code_box: false,
                                quote_bar: false,
                            },
                            h,
                        )
                    }
                }
                Block::Code { lang, code } => {
                    let fsize = (ctx.font_size * 0.85).max(12.0);
                    let detected = highlight::detect_lang(lang);
                    let mut job = highlight::highlight_block(code, detected, ctx.mode, fsize);
                    job.wrap.max_width = (ctx.max_width - 24.0).max(120.0);
                    let galley = ui.fonts_mut(|f| f.layout_job(job.clone()));
                    let h = galley.rect.height() + 22.0 + gap;
                    (
                        FlowVisual::SingleJob {
                            job,
                            pad_top: 10.0,
                            link_spans: Vec::new(),
                            code_box: true,
                            quote_bar: false,
                        },
                        h,
                    )
                }
                Block::Rule => (FlowVisual::Separator, 18.0 + gap),
                Block::Quote(_)
                | Block::List { .. }
                | Block::Table { .. }
                | Block::DisplayMath { .. }
                | Block::Image { .. } => (FlowVisual::FallbackBlock, 48.0),
            }
        }
    }
}

#[derive(Clone, Copy, Default)]
struct InlineFlags {
    italic: bool,
    strike: bool,
    code: bool,
    superscript: bool,
    subscript: bool,
}

fn flatten_inline_tokens(
    inlines: &[Inline],
    flags: InlineFlags,
    link: Option<LinkTarget>,
    out: &mut Vec<FlatToken>,
) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => {
                for tok in split_wrap_tokens(text) {
                    out.push(FlatToken::Text {
                        text: tok,
                        italic: flags.italic,
                        strike: flags.strike,
                        code: flags.code,
                        superscript: flags.superscript,
                        subscript: flags.subscript,
                        link: link.clone(),
                    });
                }
            }
            Inline::Styled {
                bold: _,
                italic: it,
                strike: st,
                code: cd,
                superscript: sup,
                subscript: sub,
                inlines,
            } => flatten_inline_tokens(
                inlines,
                InlineFlags {
                    italic: flags.italic || *it,
                    strike: flags.strike || *st,
                    code: flags.code || *cd,
                    superscript: flags.superscript || *sup,
                    subscript: flags.subscript || *sub,
                },
                link.clone(),
                out,
            ),
            Inline::Link { target, inlines } => {
                flatten_inline_tokens(inlines, flags, Some(target.clone()), out);
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
            tokens.push(ch.to_string());
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
    let color = [ink.r(), ink.g(), ink.b(), ink.a()];
    let ppp = ctx_ui.ctx().pixels_per_point();
    let key = MathTexKey {
        latex: latex.to_owned(),
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

fn measure_rich_lines(
    ui: &mut Ui,
    inlines: &[Inline],
    scale_mul: f32,
    ctx: &mut FlowDrawCtx<'_>,
) -> (Vec<MeasuredLine>, String, Vec<Rect>, f32, f32, f32) {
    let fsize = ctx.font_size * scale_mul;
    let mut tokens = Vec::new();
    flatten_inline_tokens(inlines, InlineFlags::default(), None, &mut tokens);

    let max_w = ctx.max_width.max(60.0);
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
                superscript,
                subscript,
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
                let eff_size = if superscript || subscript {
                    (fsize * 0.72).max(10.0)
                } else if code {
                    fsize * 0.92
                } else {
                    fsize
                };
                let fid = if code {
                    FontId::monospace(eff_size)
                } else {
                    FontId::proportional(eff_size)
                };
                let mut job = LayoutJob::default();
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
                        text,
                        link,
                        code_bg: code,
                    });
                    cur_x += w;
                }
            }
            FlatToken::Ruby { base, annotation } => {
                let base_galley = ui.fonts_mut(|f| {
                    f.layout_no_wrap(base.clone(), FontId::proportional(fsize), ctx.palette.ink)
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
                        base_str: base,
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
                            latex,
                        });
                        cur_x += size.x;
                    }
                } else {
                    let fallback = format!("${latex}$");
                    let galley = ui.fonts_mut(|f| {
                        f.layout_no_wrap(
                            fallback.clone(),
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
                            text: fallback,
                            link: None,
                            code_bg: true,
                        });
                        cur_x += w;
                    }
                }
            }
        }
    }

    let extra_gap = (fsize * (ctx.line_height - 1.35)).max(2.0);
    let total_h: f32 = lines
        .iter()
        .map(|l| l.ascent + l.descent + extra_gap)
        .sum::<f32>()
        .max(fsize);

    let mut plain = String::new();
    let mut char_boxes = Vec::new();
    let mut y = 0.0f32;
    for line in &lines {
        let baseline = y + line.ascent;
        let mut x = 0.0f32;
        for item in &line.items {
            match item {
                LineItem::Text { galley, text, .. } => {
                    let gw = galley.rect.width();
                    let gh = galley.rect.height();
                    let top = baseline - base_ascent;
                    let cnt = text.chars().count().max(1);
                    let cw = gw / cnt as f32;
                    for (ci, ch) in text.chars().enumerate() {
                        plain.push(ch);
                        char_boxes.push(Rect::from_min_size(
                            Pos2::new(x + ci as f32 * cw, top),
                            Vec2::new(cw, gh),
                        ));
                    }
                    x += gw;
                }
                LineItem::Ruby {
                    base,
                    rt: _,
                    base_str,
                } => {
                    let atom_w = base.rect.width().max(2.0);
                    let gh = base.rect.height();
                    let top = baseline - base_ascent;
                    let cnt = base_str.chars().count().max(1);
                    let cw = atom_w / cnt as f32;
                    for (ci, ch) in base_str.chars().enumerate() {
                        plain.push(ch);
                        char_boxes.push(Rect::from_min_size(
                            Pos2::new(x + ci as f32 * cw, top),
                            Vec2::new(cw, gh),
                        ));
                    }
                    x += atom_w;
                }
                LineItem::Math {
                    size,
                    baseline_from_top,
                    latex,
                    ..
                } => {
                    let top = baseline - baseline_from_top;
                    let cnt = latex.chars().count().max(1);
                    let cw = size.x / cnt as f32;
                    for (ci, ch) in latex.chars().enumerate() {
                        plain.push(ch);
                        char_boxes.push(Rect::from_min_size(
                            Pos2::new(x + ci as f32 * cw, top),
                            Vec2::new(cw, size.y),
                        ));
                    }
                    x += size.x;
                }
            }
        }
        y += line.ascent + line.descent + extra_gap;
    }

    (lines, plain, char_boxes, extra_gap, base_ascent, total_h)
}

fn paint_measured_lines(
    ui: &Ui,
    lines: &[MeasuredLine],
    origin: Pos2,
    base_ascent: f32,
    extra_gap: f32,
    palette: Palette,
    links_out: &mut Vec<(Rect, LinkTarget)>,
) {
    let mut y = origin.y;
    for line in lines {
        let baseline = y + line.ascent;
        let mut x = origin.x;
        for item in &line.items {
            match item {
                LineItem::Text {
                    galley,
                    text: _,
                    link,
                    code_bg,
                } => {
                    let gw = galley.rect.width();
                    let gh = galley.rect.height();
                    let pos = Pos2::new(x, baseline - base_ascent);
                    let r = Rect::from_min_size(pos, Vec2::new(gw, gh));
                    if *code_bg {
                        ui.painter().rect_filled(r.expand(2.0), 3.0, palette.code_bg);
                    }
                    ui.painter().galley(pos, galley.clone(), palette.ink);
                    if let Some(target) = link {
                        links_out.push((r, target.clone()));
                    }
                    x += gw;
                }
                LineItem::Ruby { base, rt, .. } => {
                    let bw = base.rect.width();
                    let rw = rt.rect.width();
                    let atom_w = bw.max(rw) + 2.0;
                    let base_x = x + (atom_w - bw) * 0.5;
                    let rt_x = x + (atom_w - rw) * 0.5;
                    let base_y = baseline - base_ascent;
                    let rt_y = base_y - rt.rect.height() - 1.0;
                    ui.painter()
                        .galley(Pos2::new(rt_x, rt_y), rt.clone(), palette.muted);
                    ui.painter()
                        .galley(Pos2::new(base_x, base_y), base.clone(), palette.ink);
                    x += atom_w;
                }
                LineItem::Math {
                    tex,
                    size,
                    baseline_from_top,
                    ..
                } => {
                    let pos = Pos2::new(x, baseline - baseline_from_top);
                    let r = Rect::from_min_size(pos, *size);
                    ui.painter().image(
                        *tex,
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

fn draw_inline_flow(
    ui: &mut Ui,
    inlines: &[Inline],
    scale_mul: f32,
    ctx: &mut FlowDrawCtx<'_>,
) {
    let (lines, _, _, extra_gap, base_ascent, total_h) =
        measure_rich_lines(ui, inlines, scale_mul, ctx);
    let max_w = ui.available_width().min(ctx.max_width).max(60.0);
    let (block_rect, _) = ui.allocate_exact_size(Vec2::new(max_w, total_h), Sense::hover());
    let mut links = Vec::new();
    paint_measured_lines(
        ui,
        &lines,
        block_rect.min,
        base_ascent,
        extra_gap,
        ctx.palette,
        &mut links,
    );
    for (lr, target) in links {
        let resp = ui.interact(
            lr,
            ui.scope_id()
                .with(("inline-link", lr.min.x as i32, lr.min.y as i32)),
            Sense::click(),
        );
        if resp.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
        if resp.clicked() {
            *ctx.clicked_link = Some(target);
        }
    }
}

fn draw_flow_block_fallback(ui: &mut Ui, block: &Block, ctx: &mut FlowDrawCtx<'_>) {
    match block {
        Block::Heading { level, inlines, .. } => {
            let mul = match level {
                1 => 1.65,
                2 => 1.38,
                3 => 1.2,
                _ => 1.08,
            };
            ui.add_space(6.0);
            draw_inline_flow(ui, inlines, mul, ctx);
        }
        Block::Paragraph(inlines) => {
            draw_inline_flow(ui, inlines, 1.0, ctx);
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
                        draw_flow_block_fallback(ui, b, ctx);
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
                            draw_flow_block_fallback(ui, b, ctx);
                        }
                    });
                });
            }
        }
        Block::Code { lang, code } => {
            let fsize = (ctx.font_size * 0.85).max(12.0);
            let mut job =
                highlight::highlight_block(code, highlight::detect_lang(lang), ctx.mode, fsize);
            job.wrap.max_width = (ctx.max_width - 24.0).max(120.0);
            Frame::new()
                .fill(ctx.palette.code_bg)
                .stroke(Stroke::new(1.0, ctx.palette.line))
                .corner_radius(CornerRadius::same(8))
                .inner_margin(Margin::same(10))
                .show(ui, |ui| {
                    ui.label(job);
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
                                    draw_inline_flow(ui, cell, 0.95, ctx);
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
                                    draw_inline_flow(ui, cell, 0.95, ctx);
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
                        selected_text: None,
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

#[allow(clippy::too_many_arguments)]
fn paint_galley_selection_rects(
    ui: &Ui,
    galley: &egui::Galley,
    pos: Pos2,
    c0: usize,
    c1: usize,
    color: Color32,
    underline: bool,
    underline_color: Color32,
) -> Vec<Rect> {
    let mut rects = Vec::new();
    let mut offset = 0usize;
    for row in &galley.rows {
        let len = row.row.glyphs.len();
        let row_end = offset + len + usize::from(row.ends_with_newline);
        if c1 > offset && c0 < offset + len && len > 0 {
            let s0 = c0.saturating_sub(offset).min(len);
            let s1 = c1.saturating_sub(offset).min(len);
            if s1 > s0 {
                let x0 = pos.x + row.pos.x + row.row.glyphs[s0].pos.x;
                let last = &row.row.glyphs[s1 - 1];
                let x1 = pos.x + row.pos.x + last.pos.x + last.advance_width;
                let r = Rect::from_min_max(
                    Pos2::new(x0, pos.y + row.pos.y),
                    Pos2::new(x1, pos.y + row.pos.y + row.rect().height()),
                );
                ui.painter().rect_filled(r, 1.0, color);
                if underline {
                    ui.painter().line_segment(
                        [Pos2::new(r.left(), r.bottom()), Pos2::new(r.right(), r.bottom())],
                        Stroke::new(2.0, underline_color),
                    );
                }
                rects.push(r);
            }
        }
        let _ = CCursor::new(c0);
        offset = row_end;
    }
    rects
}

fn highlight_swatch_color(name: &str) -> Color32 {
    match name {
        "green" => Color32::from_rgb(0x72, 0xc6, 0x81),
        "blue" => Color32::from_rgb(0x72, 0xb7, 0xea),
        "pink" => Color32::from_rgb(0xe9, 0x8d, 0xc2),
        _ => Color32::from_rgb(0xef, 0xc6, 0x43),
    }
}

fn highlight_fill_color(name: &str, dark: bool) -> Color32 {
    let base = highlight_swatch_color(name);
    let alpha = if dark { 92 } else { 112 };
    Color32::from_rgba_unmultiplied(base.r(), base.g(), base.b(), alpha)
}

fn chapter_plain_from_cache(cache: &FlowLayoutCache, chapter: usize) -> String {
    let start = cache.chapter_first_item.get(chapter).copied().unwrap_or(0);
    let end = cache
        .chapter_first_item
        .get(chapter + 1)
        .copied()
        .unwrap_or(cache.items.len());
    let mut out = String::new();
    for item in &cache.items[start..end] {
        if !item.plain.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(&item.plain);
        }
    }
    out
}

fn locate_anchor_in_text(haystack: &str, anchor: &Anchor) -> Option<(usize, usize)> {
    let quote = anchor.quote.as_str();
    let quote_chars = quote.chars().count();
    if quote_chars == 0 || haystack.is_empty() {
        return None;
    }
    let hay_chars: Vec<char> = haystack.chars().collect();
    let q_chars: Vec<char> = quote.chars().collect();
    if q_chars.len() > hay_chars.len() {
        return None;
    }
    if anchor.start + q_chars.len() <= hay_chars.len()
        && hay_chars[anchor.start..anchor.start + q_chars.len()] == q_chars[..]
    {
        return Some((anchor.start, anchor.start + q_chars.len()));
    }
    let mut best: Option<(usize, i64)> = None;
    let before_chars: Vec<char> = anchor.before.chars().collect();
    let after_chars: Vec<char> = anchor.after.chars().collect();
    for pos in 0..=hay_chars.len() - q_chars.len() {
        if hay_chars[pos..pos + q_chars.len()] == q_chars[..] {
            let mut score = -(pos.abs_diff(anchor.start) as i64);
            if !before_chars.is_empty() {
                let take = before_chars.len().min(pos);
                let matched = hay_chars[pos - take..pos]
                    .iter()
                    .rev()
                    .zip(before_chars.iter().rev())
                    .take_while(|(a, b)| a == b)
                    .count();
                score += (matched as i64) * 100;
            }
            if !after_chars.is_empty() {
                let end_pos = pos + q_chars.len();
                let matched = hay_chars[end_pos..]
                    .iter()
                    .zip(after_chars.iter())
                    .take_while(|(a, b)| a == b)
                    .count();
                score += (matched as i64) * 100;
            }
            if best.is_none_or(|(_, bs)| score > bs) {
                best = Some((pos, score));
            }
        }
    }
    if let Some((pos, _)) = best {
        return Some((pos, pos + q_chars.len()));
    }
    let trimmed = quote.trim();
    if !trimmed.is_empty() && trimmed != quote {
        let t_chars: Vec<char> = trimmed.chars().collect();
        if t_chars.len() <= hay_chars.len() {
            for pos in 0..=hay_chars.len() - t_chars.len() {
                if hay_chars[pos..pos + t_chars.len()] == t_chars[..] {
                    return Some((pos, pos + t_chars.len()));
                }
            }
        }
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn paint_fixed_page_annotations(
    ui: &Ui,
    page_idx: usize,
    page_rect: Rect,
    factor: f32,
    pdf_runs: &[PdfTextRun],
    word_runs: &[WordTextRun],
    annotations: &[Annotation],
    dark: bool,
) -> Vec<(String, Rect)> {
    let mut hit_rects = Vec::new();
    let has_page_ann = annotations
        .iter()
        .any(|ann| ann.parts.iter().any(|pt| pt.index == page_idx && !pt.quote.is_empty()));
    if !has_page_ann {
        return hit_rects;
    }

    let mut page_plain = String::new();
    let mut run_spans: Vec<(usize, usize, Rect)> = Vec::new();
    let mut offset = 0usize;
    if !pdf_runs.is_empty() {
        for run in pdf_runs {
            let len = run.text.chars().count();
            let rr = Rect::from_min_size(
                Pos2::new(
                    page_rect.left() + run.rect_pt[0] * factor,
                    page_rect.top() + run.rect_pt[1] * factor,
                ),
                Vec2::new(run.rect_pt[2] * factor, run.rect_pt[3] * factor),
            );
            page_plain.push_str(&run.text);
            run_spans.push((offset, offset + len, rr));
            offset += len;
        }
    } else {
        for run in word_runs {
            let len = run.text.chars().count();
            let rr = Rect::from_min_size(
                Pos2::new(
                    page_rect.left() + run.left * factor,
                    page_rect.top() + run.top * factor,
                ),
                Vec2::new(run.width * factor, (run.size * 1.2) * factor),
            );
            page_plain.push_str(&run.text);
            run_spans.push((offset, offset + len, rr));
            offset += len;
        }
    }
    if run_spans.is_empty() {
        return hit_rects;
    }

    for ann in annotations {
        let fill = highlight_fill_color(&ann.color, dark);
        let underline = highlight_swatch_color(&ann.color);
        let is_note = ann.kind == "note";
        for part in &ann.parts {
            if part.index != page_idx || part.quote.is_empty() {
                continue;
            }
            let Some((m0, m1)) = locate_anchor_in_text(&page_plain, part) else {
                continue;
            };
            for &(r0, r1, rr) in &run_spans {
                let len = r1.saturating_sub(r0);
                if len == 0 || m1 <= r0 || m0 >= r1 {
                    continue;
                }
                let c0 = m0.saturating_sub(r0).min(len);
                let c1 = m1.saturating_sub(r0).min(len);
                if c1 > c0 {
                    let f0 = c0 as f32 / len as f32;
                    let f1 = c1 as f32 / len as f32;
                    let sub = Rect::from_min_max(
                        Pos2::new(rr.left() + rr.width() * f0, rr.top()),
                        Pos2::new(rr.left() + rr.width() * f1, rr.bottom()),
                    );
                    ui.painter().rect_filled(sub, 1.5, fill);
                    if is_note {
                        ui.painter().line_segment(
                            [Pos2::new(sub.left(), sub.bottom()), Pos2::new(sub.right(), sub.bottom())],
                            Stroke::new(2.0, underline),
                        );
                    }
                    hit_rects.push((ann.id.clone(), sub));
                }
            }
        }
    }
    hit_rects
}

fn draw_chevron(painter: &egui::Painter, center: Pos2, left: bool, color: Color32) {
    let dx = if left { 2.5 } else { -2.5 };
    let tip_x = center.x - dx;
    let tail_x = center.x + dx;
    let dy = 4.5;
    let stroke = Stroke::new(1.6, color);
    painter.line_segment(
        [Pos2::new(tail_x, center.y - dy), Pos2::new(tip_x, center.y)],
        stroke,
    );
    painter.line_segment(
        [Pos2::new(tip_x, center.y), Pos2::new(tail_x, center.y + dy)],
        stroke,
    );
}

fn draw_eco_leaf_icon(painter: &egui::Painter, rect: Rect, dark: bool) {
    let leaf_color = if dark {
        Color32::from_rgb(0x7c, 0xc2, 0x96)
    } else {
        Color32::from_rgb(0x3f, 0x91, 0x5c)
    };
    let vein_color = if dark {
        Color32::from_rgb(0x24, 0x2e, 0x28)
    } else {
        Color32::from_rgb(0xef, 0xee, 0xe6)
    };
    let l = rect.left();
    let t = rect.top();
    let w = rect.width();
    let h = rect.height();
    let pts = vec![
        Pos2::new(l + w * 0.20, t + h * 0.80),
        Pos2::new(l + w * 0.22, t + h * 0.42),
        Pos2::new(l + w * 0.48, t + h * 0.16),
        Pos2::new(l + w * 0.86, t + h * 0.14),
        Pos2::new(l + w * 0.82, t + h * 0.56),
        Pos2::new(l + w * 0.54, t + h * 0.82),
    ];
    painter.add(egui::Shape::convex_polygon(pts, leaf_color, Stroke::NONE));
    painter.line_segment(
        [
            Pos2::new(l + w * 0.14, t + h * 0.88),
            Pos2::new(l + w * 0.70, t + h * 0.30),
        ],
        Stroke::new(1.15, vein_color),
    );
}

fn draw_callout_bubble(
    ctx: &Context,
    id_salt: &str,
    anchor: Rect,
    above_anchor: bool,
    title: &str,
    body: &str,
    p: Palette,
) -> bool {
    let screen = ctx.content_rect();
    let bubble_w = (screen.width() - 24.0).clamp(240.0, 356.0);
    let anchor_cx = anchor.center().x.clamp(screen.left() + 20.0, screen.right() - 20.0);
    let bubble_x = (anchor_cx - bubble_w * 0.5)
        .clamp(screen.left() + 12.0, (screen.right() - bubble_w - 12.0).max(12.0));

    egui::Area::new(Id::unique((id_salt, "backdrop")))
        .fixed_pos(screen.min)
        .order(egui::Order::Foreground)
        .show(ctx, |ui| {
            let (bg_rect, _) = ui.allocate_exact_size(screen.size(), Sense::click());
            let painter = ui.painter();
            painter.rect_filled(bg_rect, 0.0, Color32::from_rgba_unmultiplied(0x0f, 0x16, 0x12, 110));
            painter.rect_stroke(
                anchor.expand(3.0),
                8.0,
                Stroke::new(2.0, p.accent),
                StrokeKind::Outside,
            );
        });

    let bubble_id = Id::unique((id_salt, "bubble"));
    let est_h = ctx
        .data(|d| d.get_temp::<f32>(bubble_id))
        .unwrap_or(142.0);
    let arrow_h = 10.0;
    let bubble_y = if above_anchor {
        (anchor.top() - est_h - arrow_h - 4.0).max(screen.top() + 12.0)
    } else {
        (anchor.bottom() + arrow_h + 4.0).min((screen.bottom() - est_h - 12.0).max(screen.top() + 12.0))
    };

    let mut dismissed = false;
    let area_resp = egui::Area::new(bubble_id)
        .fixed_pos(Pos2::new(bubble_x, bubble_y))
        .order(egui::Order::Tooltip)
        .show(ctx, |ui| {
            Frame::new()
                .fill(p.surface)
                .stroke(Stroke::new(1.5, p.accent))
                .corner_radius(CornerRadius::same(10))
                .inner_margin(Margin::same(14))
                .shadow(egui::Shadow {
                    offset: [0, 8],
                    blur: 22,
                    spread: 0,
                    color: Color32::from_black_alpha(72),
                })
                .show(ui, |ui| {
                    ui.set_width(bubble_w - 28.0);
                    ui.label(RichText::new(title).size(15.0).strong().color(p.ink));
                    ui.add_space(6.0);
                    ui.label(RichText::new(body).size(13.5).color(p.ink));
                    ui.add_space(10.0);
                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if ui
                            .add(
                                egui::Button::new(
                                    RichText::new("我知道了").color(p.accent_ink).strong(),
                                )
                                .fill(p.accent),
                            )
                            .clicked()
                        {
                            dismissed = true;
                        }
                    });
                });
        });

    let actual_rect = area_resp.response.rect;
    if (actual_rect.height() - est_h).abs() > 1.0 {
        ctx.data_mut(|d| d.insert_temp(bubble_id, actual_rect.height()));
        ctx.request_repaint();
    }

    let arrow_x = anchor_cx.clamp(actual_rect.left() + 18.0, actual_rect.right() - 18.0);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Tooltip,
        Id::unique((id_salt, "arrow")),
    ));
    if above_anchor {
        let base_y = actual_rect.bottom() - 1.5;
        let tip = Pos2::new(arrow_x, actual_rect.bottom() + arrow_h - 1.0);
        let left = Pos2::new(arrow_x - 9.0, base_y);
        let right = Pos2::new(arrow_x + 9.0, base_y);
        painter.add(egui::Shape::convex_polygon(
            vec![left, right, tip],
            p.surface,
            Stroke::NONE,
        ));
        painter.line_segment([left, tip], Stroke::new(1.5, p.accent));
        painter.line_segment([tip, right], Stroke::new(1.5, p.accent));
    } else {
        let base_y = actual_rect.top() + 1.5;
        let tip = Pos2::new(arrow_x, actual_rect.top() - arrow_h + 1.0);
        let left = Pos2::new(arrow_x - 9.0, base_y);
        let right = Pos2::new(arrow_x + 9.0, base_y);
        painter.add(egui::Shape::convex_polygon(
            vec![left, right, tip],
            p.surface,
            Stroke::NONE,
        ));
        painter.line_segment([left, tip], Stroke::new(1.5, p.accent));
        painter.line_segment([tip, right], Stroke::new(1.5, p.accent));
    }
    dismissed
}

fn append_welcome_inline_md(job: &mut LayoutJob, text: &str, size: f32, p: Palette) {
    let bytes = text.as_bytes();
    let mut i = 0usize;
    let normal = egui::TextFormat {
        font_id: FontId::proportional(size),
        color: p.ink,
        ..Default::default()
    };
    let bold = egui::TextFormat {
        font_id: FontId::proportional(size),
        color: p.accent,
        ..Default::default()
    };
    let code = egui::TextFormat {
        font_id: FontId::monospace((size - 1.0).max(11.5)),
        color: p.accent,
        background: p.code_bg,
        ..Default::default()
    };
    while i < bytes.len() {
        if text[i..].starts_with("**")
            && let Some(end_rel) = text[i + 2..].find("**")
        {
            let inner = &text[i + 2..i + 2 + end_rel];
            job.append(inner, 0.0, bold.clone());
            i += 4 + end_rel;
            continue;
        }
        if bytes[i] == b'`'
            && let Some(end_rel) = text[i + 1..].find('`')
        {
            let inner = &text[i + 1..i + 1 + end_rel];
            job.append(&format!(" {inner} "), 0.0, code.clone());
            i += 2 + end_rel;
            continue;
        }
        let next_special = text[i..]
            .find("**")
            .into_iter()
            .chain(text[i..].find('`'))
            .filter(|&off| off > 0)
            .min()
            .unwrap_or(text.len() - i);
        let end = i + next_special.max(text[i..].chars().next().map_or(1, char::len_utf8));
        job.append(&text[i..end], 0.0, normal.clone());
        i = end;
    }
}

fn draw_welcome_intro_markdown(ui: &mut Ui, md: &str, p: Palette) -> bool {
    let mut open_repo = false;
    for raw_line in md.lines() {
        let line = raw_line.trim();
        if line.is_empty() {
            ui.add_space(6.0);
            continue;
        }
        if line.starts_with("# ") {
            continue;
        }
        if let Some(h) = line.strip_prefix("### ") {
            ui.add_space(6.0);
            ui.label(RichText::new(h).size(15.5).strong().color(p.ink));
            ui.add_space(2.0);
            continue;
        }
        if let Some(item) = line.strip_prefix("- ") {
            ui.horizontal_top(|ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("•").size(14.5).strong().color(p.accent));
                let mut job = LayoutJob::default();
                append_welcome_inline_md(&mut job, item, 14.0, p);
                ui.add(egui::Label::new(job).wrap());
            });
            ui.add_space(3.0);
            continue;
        }
        if line.contains("github.com/drliuhuan/HayakuReader") {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("GitHub 仓库：").size(14.0).color(p.muted));
                let link = ui.add(
                    egui::Button::new(
                        RichText::new("github.com/drliuhuan/HayakuReader")
                            .size(14.0)
                            .underline()
                            .color(p.accent),
                    )
                    .frame(false),
                );
                if link.hovered() {
                    ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
                }
                if link.clicked() {
                    open_repo = true;
                }
            });
            continue;
        }
        if line.starts_with('©') {
            ui.label(RichText::new(line).size(12.5).color(p.muted));
            continue;
        }
        let mut job = LayoutJob::default();
        append_welcome_inline_md(&mut job, line, 14.5, p);
        ui.add(egui::Label::new(job).wrap());
        ui.add_space(4.0);
    }
    open_repo
}

fn hit_test_char_boxes(rel: Vec2, boxes: &[Rect]) -> usize {
    if boxes.is_empty() {
        return 0;
    }
    let pt = Pos2::new(rel.x, rel.y);
    let mut best_i = 0usize;
    let mut best_d = f32::INFINITY;
    for (i, r) in boxes.iter().enumerate() {
        if r.contains(pt) {
            return i + usize::from(pt.x > r.center().x);
        }
        let dy = (pt.y - r.center().y).abs();
        let dx = (pt.x - r.center().x).abs();
        let d = dy * 4.0 + dx;
        if d < best_d {
            best_d = d;
            best_i = i;
        }
    }
    best_i
}

fn hit_test_fixed_page(
    page: usize,
    pointer: Pos2,
    page_rect: Rect,
    factor: f32,
    pdf_runs: &[PdfTextRun],
    word_runs: &[WordTextRun],
) -> Option<FixedTextPos> {
    let run_count = pdf_runs.len().max(word_runs.len());
    if run_count == 0 {
        return None;
    }
    let mut best: Option<(f32, FixedTextPos)> = None;
    for ri in 0..run_count {
        let (rr, len) = if !pdf_runs.is_empty() {
            let run = &pdf_runs[ri];
            (
                Rect::from_min_size(
                    Pos2::new(
                        page_rect.left() + run.rect_pt[0] * factor,
                        page_rect.top() + run.rect_pt[1] * factor,
                    ),
                    Vec2::new(run.rect_pt[2] * factor, run.rect_pt[3] * factor),
                ),
                run.text.chars().count().max(1),
            )
        } else {
            let run = &word_runs[ri];
            (
                Rect::from_min_size(
                    Pos2::new(
                        page_rect.left() + run.left * factor,
                        page_rect.top() + run.top * factor,
                    ),
                    Vec2::new(run.width * factor, (run.size * 1.2) * factor),
                ),
                run.text.chars().count().max(1),
            )
        };
        let expanded = rr.expand2(Vec2::new(12.0, 6.0));
        if expanded.contains(pointer) {
            let t = ((pointer.x - rr.left()) / rr.width().max(1.0)).clamp(0.0, 1.0);
            let char_idx = (t * len as f32).round() as usize;
            let dist = (pointer.y - rr.center().y).abs() * 4.0 + (pointer.x - rr.center().x).abs();
            let cand = FixedTextPos {
                page,
                run_idx: ri,
                char_idx,
            };
            if best.is_none_or(|(d, _)| dist < d) {
                best = Some((dist, cand));
            }
        }
    }
    best.map(|(_, pos)| pos)
}

fn slice_chars(text: &str, c0: usize, c1: usize) -> String {
    text.chars().skip(c0).take(c1.saturating_sub(c0)).collect()
}

fn word_runs_plain(runs: &[WordTextRun]) -> String {
    let mut out = String::new();
    for r in runs {
        if r.line_break && !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&r.text);
    }
    out
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
    let mut buf = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(img)
        .write_to(&mut buf, image::ImageFormat::Png)
        .ok()?;
    Some(buf.into_inner())
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
    let cx = x.min(src_w.saturating_sub(1));
    let cy = y.min(src_h.saturating_sub(1));
    let cw = w.min(src_w - cx).max(1);
    let ch = h.min(src_h - cy).max(1);
    let cropped = image::imageops::crop_imm(&img, cx, cy, cw, ch).to_image();
    let mut buf = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgba8(cropped)
        .write_to(&mut buf, image::ImageFormat::Png)
        .ok()?;
    Some(buf.into_inner())
}

fn truncate_str(s: &str, max_chars: usize) -> String {
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i + 1 >= max_chars {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

fn extract_snippet(text: &str, match_byte: usize) -> String {
    let mut start = match_byte.saturating_sub(40);
    while start > 0 && !text.is_char_boundary(start) {
        start -= 1;
    }
    let mut end = (match_byte + 80).min(text.len());
    while end < text.len() && !text.is_char_boundary(end) {
        end += 1;
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn find_text_rect_in_shape(shape: &egui::epaint::Shape, needle: &str) -> Option<Rect> {
        match shape {
            egui::epaint::Shape::Vec(v) => v
                .iter()
                .find_map(|s| find_text_rect_in_shape(s, needle)),
            egui::epaint::Shape::Text(t) => {
                if t.galley.text() == needle {
                    Some(Rect::from_min_size(t.pos, t.galley.size()))
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    fn collect_texts_in_shape(shape: &egui::epaint::Shape, out: &mut Vec<String>) {
        match shape {
            egui::epaint::Shape::Vec(v) => {
                for s in v {
                    collect_texts_in_shape(s, out);
                }
            }
            egui::epaint::Shape::Text(t) => out.push(t.galley.text().to_owned()),
            _ => {}
        }
    }

    fn make_test_app(data: PathBuf) -> HayakuApp {
        let (bg_tx, bg_rx) = mpsc::channel();
        HayakuApp {
            data,
            settings: Settings::default(),
            recent: Vec::new(),
            workspace: Workspace::default(),
            tabs: Vec::new(),
            active_book: None,
            background_books: HashMap::new(),
            last_sleep_check: Instant::now(),
            tab_sleep_hint_open: false,
            tab_sleep_hint_tab_id: None,
            tab_screen_rects: HashMap::new(),
            tabs_scroll_offset: 0.0,
            tabs_scroll_max: 0.0,
            scroll_active_tab_into_view: false,
            smart_invert_btn_rect: None,
            splash: SplashState::Done,
            sidebar_open: false,
            sidebar_pinned: false,
            sidebar_tab: SidebarTab::Contents,
            active_outline_idx: None,
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
            pending_scroll_y: None,
            last_scroll_y: 0.0,
            last_viewport_h: 600.0,
            last_content_h: 600.0,
            flow_cache: None,
            flow_sel_anchor: None,
            flow_sel_head: None,
            flow_sel_dragging: false,
            flow_width_dragging: false,
            fixed_sel_anchor: None,
            fixed_sel_head: None,
            fixed_sel_dragging: false,
            fixed_all_selected: false,
            last_selection_rect: None,
            math_cache: MathCache::default(),
            math_textures: HashMap::new(),
            image_textures: HashMap::new(),
            logo_texture: None,
            status_msg: None,
            context_menu: None,
            link_warning: None,
            annotation_editor: None,
            smart_invert_hint_open: false,
            install_status: None,
            show_install_dialog: false,
            install_selected: HashMap::new(),
            pending_release: None,
            show_update_dialog: false,
            update_progress: Arc::new(Mutex::new(None)),
            update_downloading: false,
            instance_lock: None,
            bg_rx,
            bg_tx,
            bg_stop: Arc::new(AtomicBool::new(false)),
            scroll_fraction: 0.0,
            current_unit: 0,
        }
    }

    #[test]
    fn install_modal_cancel_button_responds_to_click() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());
        app.install_status = Some(platform::InstallationStatus {
            installed: false,
            installation_exists: false,
            path: dir.path().join("HayakuReader.exe"),
            associations: vec![".pdf".to_owned()],
        });
        app.show_install_dialog = true;

        let ctx = Context::default();
        install_system_fonts(&ctx);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1024.0, 720.0));

        // Warmup frames so anchored Modal finishes sizing and fade-in before reading shapes.
        let mut shapes = Vec::new();
        for t in [0.0_f64, 0.5_f64] {
            let raw = egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(t),
                ..Default::default()
            };
            let mut out = ctx.run_ui(raw, |ui| {
                egui::CentralPanel::default().show(ui, |_ui| {});
                app.draw_resize_edges(ui.ctx());
                app.draw_popups(ui.ctx());
            });
            shapes = std::mem::take(&mut out.shapes);
            out.drop_without_applying_deltas();
        }

        let cancel_rect = shapes
            .iter()
            .find_map(|cs| find_text_rect_in_shape(&cs.shape, "取消"))
            .unwrap_or_else(|| {
                let mut seen = Vec::new();
                for cs in &shapes {
                    collect_texts_in_shape(&cs.shape, &mut seen);
                }
                panic!("expected '取消' button text in rendered shapes; rendered: {seen:?}");
            });
        let click_pos = cancel_rect.center();

        // Press left mouse button on "取消".
        let raw_down = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.6),
            events: vec![
                egui::Event::PointerMoved(click_pos),
                egui::Event::PointerButton {
                    pos: click_pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
            ..Default::default()
        };
        let out_down = ctx.run_ui(raw_down, |ui| {
            egui::CentralPanel::default().show(ui, |_ui| {});
            app.draw_resize_edges(ui.ctx());
            app.draw_popups(ui.ctx());
        });
        out_down.drop_without_applying_deltas();

        // Release left mouse button on "取消".
        let raw_up = egui::RawInput {
            screen_rect: Some(screen_rect),
            time: Some(0.7),
            events: vec![egui::Event::PointerButton {
                pos: click_pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            }],
            ..Default::default()
        };
        let out_up = ctx.run_ui(raw_up, |ui| {
            egui::CentralPanel::default().show(ui, |_ui| {});
            app.draw_resize_edges(ui.ctx());
            app.draw_popups(ui.ctx());
        });
        out_up.drop_without_applying_deltas();

        assert!(
            !app.show_install_dialog,
            "clicking '取消' on install modal should close show_install_dialog"
        );
        assert!(
            app.settings.install_dismissed,
            "clicking '取消' on install modal should persist install_dismissed"
        );
    }

    #[test]
    fn auto_sidebar_mode_and_pin_switch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());

        let make_flow_book = |with_outline: bool, with_notes: bool| -> OpenBook {
            let outline = if with_outline {
                vec![OutlineEntry {
                    title: "Chapter 1".to_owned(),
                    index: 0,
                    fragment: String::new(),
                }]
            } else {
                Vec::new()
            };
            let mut state = BookState::default();
            if with_notes {
                state.annotations.push(Annotation {
                    id: "b1".to_owned(),
                    kind: "bookmark".to_owned(),
                    text: "Bookmark".to_owned(),
                    ..Default::default()
                });
            }
            OpenBook {
                id: "id1".to_owned(),
                title: "Book".to_owned(),
                digest: "d".to_owned(),
                kind: "txt",
                source: LoadedSource::Flow(Arc::new(FlowDocument {
                    kind: "txt",
                    chapters: vec![crate::flow::FlowChapter {
                        path: "ch1".to_owned(),
                        title: "Chapter 1".to_owned(),
                        blocks: vec![Block::Paragraph(vec![Inline::Text("Hello".to_owned())])],
                        anchors: HashMap::new(),
                        raw_source: "Hello".to_owned(),
                    }],
                    outline,
                    resources: HashMap::new(),
                })),
                state,
            }
        };

        // Doc with outline opens sidebar on Contents in auto mode.
        app.active_book = Some(make_flow_book(true, false));
        app.sidebar_tab = SidebarTab::Recent;
        app.sync_auto_sidebar(true);
        assert!(app.sidebar_open);
        assert_eq!(app.sidebar_tab, SidebarTab::Contents);

        // Doc without outline but with annotations also opens sidebar on Contents.
        app.active_book = Some(make_flow_book(false, true));
        app.sidebar_tab = SidebarTab::Notes;
        app.sync_auto_sidebar(true);
        assert!(app.sidebar_open);
        assert_eq!(app.sidebar_tab, SidebarTab::Contents);

        // Doc without outline or annotations hides sidebar in auto mode.
        app.active_book = Some(make_flow_book(false, false));
        app.sync_auto_sidebar(true);
        assert!(!app.sidebar_open);

        // Pinning keeps sidebar open even without outline/annotations, and unpinning returns to auto hide.
        app.sidebar_pinned = true;
        app.sync_auto_sidebar(false);
        assert!(app.sidebar_open);
        app.sidebar_pinned = false;
        app.sync_auto_sidebar(false);
        assert!(!app.sidebar_open);
    }

    #[test]
    fn tabs_bar_hover_does_not_set_text_cursor() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());
        let blocks = (0..60)
            .map(|i| Block::Paragraph(vec![Inline::Text(format!("Line {i} sample body text"))]))
            .collect();
        app.tabs.push(TabEntry {
            id: "t1".to_owned(),
            title: "Sample Tab".to_owned(),
            last_used: 1,
        });
        app.active_book = Some(OpenBook {
            id: "t1".to_owned(),
            title: "Sample Tab".to_owned(),
            digest: "d".to_owned(),
            kind: "txt",
            source: LoadedSource::Flow(Arc::new(FlowDocument {
                kind: "txt",
                chapters: vec![crate::flow::FlowChapter {
                    path: "body".to_owned(),
                    title: "Body".to_owned(),
                    blocks,
                    anchors: HashMap::new(),
                    raw_source: String::new(),
                }],
                outline: Vec::new(),
                resources: HashMap::new(),
            })),
            state: BookState::default(),
        });
        app.pending_scroll_y = Some(120.0);

        let ctx = Context::default();
        install_system_fonts(&ctx);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(1024.0, 720.0));
        let hover_pos = Pos2::new(70.0, 60.0);
        let mut last_cursor = CursorIcon::Default;
        for t in [0.0_f64, 0.2_f64] {
            let raw = egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(t),
                events: vec![egui::Event::PointerMoved(hover_pos)],
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                app.draw_top_bar(ui);
                app.draw_tabs_bar(ui);
                app.draw_bottom_bar(ui);
                let p = app.palette();
                CentralPanel::default()
                    .frame(Frame::new().fill(p.paper))
                    .show(ui, |ui| app.draw_flow_viewer(ui));
            });
            last_cursor = out.platform_output.cursor_icon;
            out.drop_without_applying_deltas();
        }
        assert_ne!(
            last_cursor,
            CursorIcon::Text,
            "hovering tabs bar over a scrolled Flow document must not show Text cursor"
        );
    }

    #[test]
    fn drain_inbox_dir_reads_and_clears_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inbox = dir.path().join("inbox");
        fs::create_dir_all(&inbox).expect("mkdir");
        let p1 = dir.path().join("a.epub");
        let p2 = dir.path().join("b.pdf");
        storage::write_json(&inbox.join("1.json"), &vec![p1.clone()]).expect("write 1");
        storage::write_json(&inbox.join("2.json"), &vec![p2.clone()]).expect("write 2");

        let mut got = drain_inbox_dir(&inbox).expect("expected Some(paths)");
        got.sort();
        let mut expected = vec![p1, p2];
        expected.sort();
        assert_eq!(got, expected);
        assert!(drain_inbox_dir(&inbox).is_none());
    }

    fn make_dummy_flow_book(id: &str, title: &str) -> OpenBook {
        OpenBook {
            id: id.to_owned(),
            title: title.to_owned(),
            digest: format!("digest-{id}"),
            kind: "txt",
            source: LoadedSource::Flow(Arc::new(FlowDocument {
                kind: "txt",
                chapters: vec![crate::flow::FlowChapter {
                    path: "body".to_owned(),
                    title: "Body".to_owned(),
                    blocks: vec![Block::Paragraph(vec![Inline::Text(format!(
                        "Content of {title}"
                    ))])],
                    anchors: HashMap::new(),
                    raw_source: format!("Content of {title}"),
                }],
                outline: Vec::new(),
                resources: HashMap::new(),
            })),
            state: BookState::default(),
        }
    }

    #[test]
    fn tab_sleep_enforces_five_recent_limit_and_five_minute_idle_and_first_hint() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());
        let base_ms = 1_000_000_000u64;

        // 6 tabs: t0..t4 in background, t5 active; t0 is oldest (rank 5) -> must sleep
        for i in 0..6 {
            let id = format!("t{i}");
            app.tabs.push(TabEntry {
                id: id.clone(),
                title: format!("Doc {i}"),
                last_used: base_ms + (i as u64) * 1_000,
            });
            if i < 5 {
                app.background_books
                    .insert(id.clone(), make_dummy_flow_book(&id, &format!("Doc {i}")));
            } else {
                app.active_book = Some(make_dummy_flow_book(&id, &format!("Doc {i}")));
            }
        }

        let slept = app.sleep_inactive_tabs_at(base_ms + 10_000);
        assert_eq!(slept, vec!["t0".to_owned()]);
        assert!(app.is_tab_sleeping("t0"));
        assert!(!app.is_tab_sleeping("t1"));
        assert!(!app.is_tab_sleeping("t5"));
        assert!(app.tab_sleep_hint_open, "first sleep should open callout hint");
        assert_eq!(app.tab_sleep_hint_tab_id.as_deref(), Some("t0"));

        // Mark hint seen; advance 5 minutes -> remaining background tabs t1..t4 must sleep, active t5 stays awake
        app.settings.tab_sleep_hint_seen = true;
        app.tab_sleep_hint_open = false;
        let idle_slept = app.sleep_inactive_tabs_at(base_ms + 10_000 + TAB_SLEEP_IDLE_MS);
        assert_eq!(idle_slept.len(), 4);
        for i in 1..5 {
            assert!(app.is_tab_sleeping(&format!("t{i}")));
        }
        assert!(!app.is_tab_sleeping("t5"));
        assert!(!app.tab_sleep_hint_open, "hint must not reopen once seen");
    }

    #[test]
    fn restore_session_only_loads_active_tab_and_keeps_others_sleeping() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f1 = dir.path().join("one.txt");
        let f2 = dir.path().join("two.txt");
        let f3 = dir.path().join("three.txt");
        fs::write(&f1, "hello one").expect("write f1");
        fs::write(&f2, "hello two").expect("write f2");
        fs::write(&f3, "hello three").expect("write f3");
        let f1 = storage::dunce_path(&f1).expect("dunce f1");
        let f2 = storage::dunce_path(&f2).expect("dunce f2");
        let f3 = storage::dunce_path(&f3).expect("dunce f3");

        let mut app = make_test_app(dir.path().to_path_buf());
        app.recent = vec![
            Recent {
                id: "id1".to_owned(),
                path: f1,
                digest: String::new(),
            },
            Recent {
                id: "id2".to_owned(),
                path: f2,
                digest: String::new(),
            },
            Recent {
                id: "id3".to_owned(),
                path: f3,
                digest: String::new(),
            },
        ];
        app.tabs = vec![
            TabEntry {
                id: "id1".to_owned(),
                title: "one.txt".to_owned(),
                last_used: 10,
            },
            TabEntry {
                id: "id2".to_owned(),
                title: "two.txt".to_owned(),
                last_used: 20,
            },
            TabEntry {
                id: "id3".to_owned(),
                title: "three.txt".to_owned(),
                last_used: 30,
            },
        ];
        app.workspace.active = Some("id2".to_owned());
        app.restore_active_session_tab_only();

        assert_eq!(app.active_book.as_ref().map(|b| b.id.as_str()), Some("id2"));
        assert!(app.background_books.is_empty());
        assert!(app.is_tab_sleeping("id1"));
        assert!(!app.is_tab_sleeping("id2"));
        assert!(app.is_tab_sleeping("id3"));
    }

    #[test]
    fn smart_invert_only_applies_to_pdf_and_word_not_flow() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());
        app.settings.theme = "dark".to_owned();
        app.settings.smart_invert = true;
        app.active_book = Some(make_dummy_flow_book("flow1", "novel.epub"));

        assert!(!app.active_book_uses_smart_invert());
        app.maybe_trigger_smart_invert_hint();
        assert!(!app.smart_invert_hint_open);

        let ctx = Context::default();
        install_system_fonts(&ctx);
        let raw = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(Pos2::ZERO, Vec2::new(1024.0, 720.0))),
            time: Some(0.0),
            ..Default::default()
        };
        let out = ctx.run_ui(raw, |ui| {
            app.draw_bottom_bar(ui);
        });
        out.drop_without_applying_deltas();
        assert!(
            app.smart_invert_btn_rect.is_none(),
            "Flow document must not display the smart-invert toggle button"
        );
    }

    #[test]
    fn tabs_bar_auto_scrolls_to_reveal_offscreen_active_tab() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut app = make_test_app(dir.path().to_path_buf());
        for i in 0..14 {
            app.tabs.push(TabEntry {
                id: format!("tab-{i}"),
                title: format!("LongDocumentTitle-{i}.epub"),
                last_used: i as u64,
            });
        }
        app.active_book = Some(make_dummy_flow_book("tab-13", "LongDocumentTitle-13.epub"));
        app.tabs_scroll_offset = 0.0;
        app.scroll_active_tab_into_view = true;

        let ctx = Context::default();
        install_system_fonts(&ctx);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(640.0, 480.0));
        for t in [0.0_f64, 0.1_f64] {
            let raw = egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(t),
                ..Default::default()
            };
            let out = ctx.run_ui(raw, |ui| {
                app.draw_tabs_bar(ui);
                CentralPanel::default().show(ui, |_ui| {});
            });
            out.drop_without_applying_deltas();
        }
        assert!(
            app.tabs_scroll_max > 100.0,
            "14 tabs in a 640px window should overflow horizontally"
        );
        assert!(
            app.tabs_scroll_offset > 0.0,
            "switching to offscreen rightmost tab should auto-scroll tabs_scroll_offset right"
        );
    }

    #[test]
    fn splash_closes_immediately_after_associated_file_loads() {
        let dir = tempfile::tempdir().expect("tempdir");
        let f1 = dir.path().join("first.txt");
        let f2 = dir.path().join("second.txt");
        fs::write(&f1, "alpha").expect("write f1");
        fs::write(&f2, "beta").expect("write f2");

        let mut app = make_test_app(dir.path().to_path_buf());
        app.splash = SplashState::LoadingFile {
            primary: f2,
            extra_sleeping: vec![f1],
            frames_rendered: 0,
        };

        let ctx = Context::default();
        install_system_fonts(&ctx);
        let screen_rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(800.0, 600.0));

        // Frame 1 renders splash screen
        let out1 = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(0.0),
                ..Default::default()
            },
            |ui| {
                assert!(app.step_splash(ui));
            },
        );
        out1.drop_without_applying_deltas();
        assert!(app.active_book.is_none());

        // Frame 2 loads file and dismisses splash immediately, keeping extra file sleeping
        let out2 = ctx.run_ui(
            egui::RawInput {
                screen_rect: Some(screen_rect),
                time: Some(0.05),
                ..Default::default()
            },
            |ui| {
                assert!(!app.step_splash(ui));
            },
        );
        out2.drop_without_applying_deltas();
        assert!(matches!(app.splash, SplashState::Done));
        assert_eq!(app.tabs.len(), 2);
        assert_eq!(
            app.active_book.as_ref().map(|b| b.title.as_str()),
            Some("second.txt")
        );
        assert!(app.background_books.is_empty());
    }

    #[test]
    fn locate_anchor_in_text_disambiguates_using_context() {
        let text = "苹果很好吃，香蕉也很甜，苹果很脆。";
        let anchor = Anchor {
            index: 0,
            start: 5,
            quote: "苹果".to_owned(),
            digest: String::new(),
            before: "也很甜，".to_owned(),
            after: "很脆".to_owned(),
        };
        let found = locate_anchor_in_text(text, &anchor).expect("should locate second occurrence");
        assert_eq!(slice_chars(text, found.0, found.1), "苹果");
        assert!(found.0 > 6);
    }
}
