pub(crate) mod rich;

// CSS print sizes use 96 px/in and 72 pt/in: 14 px = 10.5 pt.
const DEFAULT_FLOW_PRINT_FONT_PX: f32 = 14.0;
use crate::{flow::FlowDocument, storage::Annotation};
use egui::{ColorImage, Context, TextureHandle, TextureOptions, Vec2};
use serde::{Deserialize, Serialize};
use std::os::windows::process::CommandExt;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};

#[derive(Clone)]
pub enum Input {
    Pdf(PathBuf),
    Word(PathBuf),
    Flow(Arc<FlowDocument>, bool),
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PrintSettings {
    pub printer: String,
    pub paper: String,
    pub landscape: bool,
    pub copies: u16,
    pub duplex: u8,
    pub color: bool,
    pub margin_mm: f32,
    pub highlights: bool,
    pub notes: bool,
    pub first_note_left: bool,
    pub range: String,
    pub output: String,
}
impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            printer: String::new(),
            paper: "A4".into(),
            landscape: false,
            copies: 1,
            duplex: 0,
            color: true,
            margin_mm: 25.0,
            highlights: true,
            notes: false,
            first_note_left: false,
            range: String::new(),
            output: String::new(),
        }
    }
}

#[derive(Clone, Serialize)]
pub(crate) struct Mark {
    key: String,
    start: usize,
    end: usize,
    text: String,
    color: String,
    kind: String,
    rects: Vec<[f32; 4]>,
}
#[derive(Clone, Serialize)]
struct SourcePage {
    image: String,
    lazy: bool,
    width: f32,
    height: f32,
    marks: Vec<Mark>,
}
#[derive(Serialize)]
struct Source {
    reflow: bool,
    title: String,
    font_size: f32,
    pages: Vec<SourcePage>,
}
struct Prepared {
    dir: tempfile::TempDir,
    printers: Vec<String>,
    input: Input,
    title: String,
    annotations: Vec<Annotation>,
    font_size: f32,
}
enum LayoutKind {
    Pdf(PathBuf),
    Word(PathBuf, [f32; 2]),
    Flow(rich::RichLayout),
}
struct PrintLayout {
    kind: LayoutKind,
    source: Source,
    output: PathBuf,
    render_lock: Mutex<()>,
}
#[derive(Clone)]
struct Preview {
    pages: Vec<PathBuf>,
    settings: PrintSettings,
    _prepared: Arc<Prepared>,
    layout: Option<Arc<PrintLayout>>,
    sheet_sources: Vec<usize>,
    loaded: usize,
}
#[derive(Deserialize)]
struct SheetPlan {
    source_page: usize,
}

fn run_engine(
    prepared: &Prepared,
    mode: &str,
    settings: &Path,
    output: &Path,
) -> Result<(), String> {
    run_engine_range(prepared, mode, settings, output, 0, 5)
}

fn run_engine_range(
    prepared: &Prepared,
    mode: &str,
    settings: &Path,
    output: &Path,
    start: usize,
    count: usize,
) -> Result<(), String> {
    let job = if output.join("source.json").exists() {
        output.join("source.json")
    } else {
        prepared.dir.path().join("source.json")
    };
    let result = Command::new("powershell.exe")
        .creation_flags(0x08000000)
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(prepared.dir.path().join("print-engine.ps1"))
        .arg("-Mode")
        .arg(mode)
        .arg("-Job")
        .arg(job)
        .arg("-Settings")
        .arg(settings)
        .arg("-OutputDirectory")
        .arg(output)
        .arg("-PageStart")
        .arg(start.to_string())
        .arg("-PageCount")
        .arg(count.to_string())
        .output()
        .map_err(|error| error.to_string())?;
    if !result.status.success() {
        return Err(String::from_utf8_lossy(&result.stderr).trim().to_owned());
    }
    Ok(())
}

fn save_image(
    dir: &Path,
    name: &str,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
) -> Result<String, String> {
    let path = dir.join(name);
    image::RgbaImage::from_raw(width, height, pixels)
        .ok_or("无效页面像素")?
        .save(&path)
        .map_err(|error| error.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

fn marks_for(text: &str, index: usize, annotations: &[Annotation], code: bool) -> Vec<Mark> {
    let mut marks = Vec::new();
    for (annotation_index, annotation) in annotations.iter().enumerate() {
        if (annotation.view_mode.as_deref() == Some("code")) != code
            || annotation.kind == "bookmark"
        {
            continue;
        }
        if annotation.parts.is_empty()
            && annotation.kind == "note"
            && let Some(position) = annotation
                .position
                .as_ref()
                .filter(|position| position.index == index)
        {
            let count = text.chars().count();
            let point = ((position.fraction.clamp(0.0, 1.0) * count as f32) as usize)
                .min(count.saturating_sub(1));
            let start = text
                .chars()
                .take(point)
                .collect::<String>()
                .encode_utf16()
                .count();
            marks.push(Mark {
                key: format!("{index}-{annotation_index}-position"),
                start,
                end: start + 1,
                text: annotation.text.clone(),
                color: annotation.color.clone(),
                kind: annotation.kind.clone(),
                rects: Vec::new(),
            });
        }
        for (part_index, anchor) in annotation
            .parts
            .iter()
            .enumerate()
            .filter(|(_, anchor)| anchor.index == index)
        {
            if let Some((start_char, _)) = crate::app::locate_anchor_in_text(text, anchor) {
                let offset = text
                    .char_indices()
                    .nth(start_char)
                    .map_or(text.len(), |(offset, _)| offset);
                let start = text[..offset].encode_utf16().count();
                marks.push(Mark {
                    key: format!("{index}-{annotation_index}-{part_index}"),
                    start,
                    end: start + anchor.quote.encode_utf16().count(),
                    text: annotation.text.clone(),
                    color: annotation.color.clone(),
                    kind: annotation.kind.clone(),
                    rects: Vec::new(),
                });
            }
        }
    }
    marks
}

fn prepare(
    input: Input,
    title: String,
    annotations: Vec<Annotation>,
    font_size: f32,
) -> Result<Arc<Prepared>, String> {
    let dir = tempfile::tempdir().map_err(|error| error.to_string())?;
    fs::write(
        dir.path().join("print-engine.ps1"),
        format!("\u{feff}{}", include_str!("../assets/print-engine.ps1")),
    )
    .map_err(|error| error.to_string())?;
    let mut prepared = Prepared {
        dir,
        input,
        title,
        annotations,
        font_size,
        printers: Vec::new(),
    };
    if run_engine(&prepared, "printers", Path::new(""), prepared.dir.path()).is_ok()
        && let Ok(printers) = crate::storage::read_json(&prepared.dir.path().join("printers.json"))
    {
        prepared.printers = printers;
    }
    Ok(Arc::new(prepared))
}

fn page_dimensions(settings: &PrintSettings) -> (u32, u32, u32, u32) {
    let (mut width, mut height) = match settings.paper.as_str() {
        "A5" => (583, 827),
        "Letter" => (850, 1100),
        "Legal" => (850, 1400),
        _ => (827, 1169),
    };
    if settings.landscape {
        std::mem::swap(&mut width, &mut height);
    }
    let margin = (settings.margin_mm / 25.4 * 150.0) as u32;
    let full_width = (width as f32 * 1.5) as u32 - 2 * margin;
    let full_height = (height as f32 * 1.5) as u32 - 2 * margin;
    let body_width = full_width
        - if settings.notes {
            (full_width as f32 * 0.25) as u32 + 24
        } else {
            0
        };
    (full_width, full_height, body_width, full_height)
}

fn fixed_page_marks(runs: &[(String, [f32; 4])], marks: &[Mark], offset: usize) -> Vec<Mark> {
    let mut result = Vec::new();
    for mark in marks {
        let mut current = offset;
        let mut rects = Vec::new();
        for (text, rect) in runs {
            let end = current + text.encode_utf16().count();
            if mark.start < end && mark.end > current {
                let length = (end - current).max(1) as f32;
                let start = mark.start.saturating_sub(current) as f32 / length;
                let stop = (mark.end.min(end) - current) as f32 / length;
                rects.push([
                    rect[0] + rect[2] * start,
                    rect[1],
                    rect[2] * (stop - start),
                    rect[3],
                ]);
            }
            current = end;
        }
        if !rects.is_empty() {
            let mut mark = mark.clone();
            mark.rects = rects;
            result.push(mark);
        }
    }
    result
}

fn build_layout(
    prepared: &Prepared,
    settings: &PrintSettings,
    output: &Path,
) -> Result<Arc<PrintLayout>, String> {
    let (_, _, width, height) = page_dimensions(settings);
    let mut pages = Vec::new();
    let reflow = !matches!(prepared.input, Input::Pdf(_));
    let source_page = |index: usize, width: f32, height: f32, marks: Vec<Mark>| SourcePage {
        image: output
            .join(format!("source-{index:05}.png"))
            .to_string_lossy()
            .into_owned(),
        lazy: true,
        width,
        height,
        marks,
    };
    let kind = match &prepared.input {
        Input::Flow(document, code) => {
            let marks = document
                .chapters
                .iter()
                .enumerate()
                .map(|(index, chapter)| {
                    marks_for(
                        &if *code {
                            chapter.raw_source.clone()
                        } else {
                            chapter.plain_text()
                        },
                        index,
                        &prepared.annotations,
                        *code,
                    )
                })
                .collect::<Vec<_>>();
            let (blocks, textures) =
                crate::app::print_flow_scenes(document, *code, width as f32, prepared.font_size);
            let rich = rich::RichLayout::paginate(blocks, textures, width, height, &marks);
            for (index, page) in rich.pages.iter().enumerate() {
                pages.push(source_page(
                    index,
                    width as f32,
                    height as f32,
                    page.marks.clone(),
                ));
            }
            LayoutKind::Flow(rich)
        }
        Input::Pdf(path) => {
            let pdf = crate::pdf::PdfDoc::open(path)?;
            for (index, &[width, height]) in pdf.page_sizes().iter().enumerate() {
                let marks = if prepared.annotations.is_empty() {
                    Vec::new()
                } else {
                    let runs = pdf
                        .page_text_runs(index)
                        .into_iter()
                        .map(|run| (run.text, run.rect_pt))
                        .collect::<Vec<_>>();
                    let text = runs
                        .iter()
                        .map(|(text, _)| text.as_str())
                        .collect::<String>();
                    fixed_page_marks(
                        &runs,
                        &marks_for(&text, index, &prepared.annotations, false),
                        0,
                    )
                };
                pages.push(source_page(index, width, height, marks));
            }
            LayoutKind::Pdf(path.clone())
        }
        Input::Word(path) => {
            let mut marks = Vec::new();
            if !prepared.annotations.is_empty() {
                let original = crate::word::Word::open(path)?;
                let mut offset = 0;
                for index in 0..original.metadata.pages.len() {
                    let text = original
                        .text_runs(index)?
                        .iter()
                        .map(|run| run.text.as_str())
                        .collect::<String>();
                    for mut mark in marks_for(&text, index, &prepared.annotations, false) {
                        mark.start += offset;
                        mark.end += offset;
                        marks.push(mark);
                    }
                    offset += text.encode_utf16().count();
                }
            }
            let size = [width as f32 * 72.0 / 150.0, height as f32 * 72.0 / 150.0];
            let word = crate::word::Word::open_for_print(path, size)?;
            let mut offset = 0;
            for (index, &[width, height]) in word.metadata.pages.iter().enumerate() {
                let runs = word
                    .text_runs(index)?
                    .into_iter()
                    .map(|run| (run.text, [run.left, run.top, run.width, run.size * 1.25]))
                    .collect::<Vec<_>>();
                let page_marks = fixed_page_marks(&runs, &marks, offset);
                offset += runs
                    .iter()
                    .map(|(text, _)| text.encode_utf16().count())
                    .sum::<usize>();
                pages.push(source_page(index, width, height, page_marks));
            }
            LayoutKind::Word(path.clone(), size)
        }
    };
    let source = Source {
        reflow,
        title: prepared.title.clone(),
        font_size: prepared.font_size,
        pages,
    };
    crate::storage::write_json(&output.join("source.json"), &source)
        .map_err(|error| error.to_string())?;
    Ok(Arc::new(PrintLayout {
        kind,
        source,
        output: output.to_owned(),
        render_lock: Mutex::new(()),
    }))
}

fn preview_target(page: usize, total: usize) -> usize {
    if page == 0 {
        5.min(total)
    } else {
        page.saturating_add(6).min(total)
    }
}

fn render_preview_range(preview: &Preview, start: usize, end: usize) -> Result<(), String> {
    let layout = preview.layout.as_ref().ok_or("缺少打印分页数据")?;
    let _guard = layout
        .render_lock
        .lock()
        .map_err(|error| error.to_string())?;
    render_preview_range_unlocked(preview, start, end)
}

fn render_preview_range_unlocked(
    preview: &Preview,
    start: usize,
    end: usize,
) -> Result<(), String> {
    let layout = preview.layout.as_ref().ok_or("缺少打印分页数据")?;
    let mut sources = preview.sheet_sources[start..end].to_vec();
    sources.sort_unstable();
    sources.dedup();
    sources.retain(|&index| !Path::new(&layout.source.pages[index].image).exists());
    match &layout.kind {
        LayoutKind::Flow(rich) => {
            for index in sources {
                rich.render(index)?
                    .save(&layout.source.pages[index].image)
                    .map_err(|error| error.to_string())?;
            }
        }
        LayoutKind::Pdf(path) => {
            if !sources.is_empty() {
                let pdf = crate::pdf::PdfDoc::open(path)?;
                for index in sources {
                    let page = pdf.render_rgba(index, 150.0 / 72.0)?;
                    save_image(
                        &layout.output,
                        &format!("source-{index:05}.png"),
                        page.width,
                        page.height,
                        page.rgba,
                    )?;
                }
            }
        }
        LayoutKind::Word(path, size) => {
            if !sources.is_empty() {
                let word = crate::word::Word::open_for_print(path, *size)?;
                for index in sources {
                    let (width, height, rgba) = word.render_rgba(index, Some(150.0 / 72.0))?;
                    save_image(
                        &layout.output,
                        &format!("source-{index:05}.png"),
                        width,
                        height,
                        rgba,
                    )?;
                }
            }
        }
    }
    run_engine_range(
        &preview._prepared,
        "render",
        &layout.output.join("settings.json"),
        &layout.output,
        start,
        end - start,
    )
}

fn create_preview(
    prepared: Arc<Prepared>,
    settings: PrintSettings,
    output: PathBuf,
) -> Result<Preview, String> {
    fs::create_dir_all(&output).map_err(|error| error.to_string())?;
    crate::storage::write_json(&output.join("settings.json"), &settings)
        .map_err(|error| error.to_string())?;
    let layout = build_layout(&prepared, &settings, &output)?;
    run_engine(&prepared, "plan", &output.join("settings.json"), &output)?;
    let names: Vec<String> =
        crate::storage::read_json(&output.join("pages.json")).map_err(|error| error.to_string())?;
    let plan: Vec<SheetPlan> = crate::storage::read_json(&output.join("layout.json"))
        .map_err(|error| error.to_string())?;
    let preview = Preview {
        loaded: names.len().min(5),
        pages: names.iter().map(|name| output.join(name)).collect(),
        settings,
        _prepared: prepared,
        layout: Some(layout),
        sheet_sources: plan.iter().map(|sheet| sheet.source_page).collect(),
    };
    render_preview_range(&preview, 0, preview.loaded)?;
    Ok(preview)
}

pub fn selected_pages(range: &str, total: usize) -> Result<Vec<usize>, String> {
    if range.trim().is_empty() {
        return Ok((0..total).collect());
    }
    let mut pages = Vec::new();
    for part in range.split(',') {
        let limits = part.trim().split('-').map(str::trim).collect::<Vec<_>>();
        let start = limits[0]
            .parse::<usize>()
            .map_err(|_| "页码格式示例：1-3,5")?;
        let end = if limits.len() == 1 {
            start
        } else if limits.len() == 2 {
            limits[1].parse::<usize>().map_err(|_| "无效结束页码")?
        } else {
            return Err("无效页码范围".into());
        };
        if start == 0 || end < start || end > total {
            return Err("页码超出预览范围".into());
        }
        for page in start - 1..end {
            if !pages.contains(&page) {
                pages.push(page);
            }
        }
    }
    Ok(pages)
}

pub struct PrintDialog {
    title: String,
    settings: PrintSettings,
    source_rx: mpsc::Receiver<Result<Arc<Prepared>, String>>,
    prepared: Option<Arc<Prepared>>,
    preview_rx: Option<mpsc::Receiver<Result<Preview, String>>>,
    preview: Option<Preview>,
    requested: Option<PrintSettings>,
    page: usize,
    texture: Option<(PathBuf, TextureHandle)>,
    error: Option<String>,
    print_rx: Option<mpsc::Receiver<Result<(), String>>>,
    message: String,
}

impl PrintDialog {
    pub fn new(input: Input, title: String, annotations: Vec<Annotation>) -> Self {
        let (tx, source_rx) = mpsc::channel();
        let source_title = title.clone();
        thread::spawn(move || {
            let _ = tx.send(prepare(
                input,
                source_title,
                annotations,
                DEFAULT_FLOW_PRINT_FONT_PX,
            ));
        });
        Self {
            title,
            settings: PrintSettings::default(),
            source_rx,
            prepared: None,
            preview_rx: None,
            preview: None,
            requested: None,
            page: 0,
            texture: None,
            error: None,
            print_rx: None,
            message: String::new(),
        }
    }

    fn poll(&mut self, ctx: &Context) {
        if let Ok(result) = self.source_rx.try_recv() {
            match result {
                Ok(prepared) => {
                    self.settings.printer = prepared.printers.first().cloned().unwrap_or_default();
                    self.prepared = Some(prepared);
                }
                Err(error) => self.error = Some(error),
            }
        }
        if let Some(result) = self.preview_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.preview_rx = None;
            match result {
                Ok(preview) => {
                    self.page = self.page.min(preview.pages.len().saturating_sub(1));
                    self.preview = Some(preview);
                    self.texture = None;
                }
                Err(error) => self.error = Some(error),
            }
        }
        if self.preview_rx.is_none()
            && self.requested.as_ref() != Some(&self.settings)
            && let Some(prepared) = self.prepared.clone()
        {
            let settings = self.settings.clone();
            self.requested = Some(settings.clone());
            self.error = None;
            let (tx, rx) = mpsc::channel();
            self.preview_rx = Some(rx);
            thread::spawn(move || {
                let output = prepared
                    .dir
                    .path()
                    .join(format!("preview-{}", uuid::Uuid::new_v4()));
                let result = create_preview(prepared, settings, output);
                let _ = tx.send(result);
            });
        }
        if self.preview_rx.is_none()
            && self.print_rx.is_none()
            && let Some(preview) = self.preview.as_ref()
            && preview.settings == self.settings
        {
            // The initial batch is exactly five pages. Once the user advances,
            // keep the current page and its next five pages available.
            let target = preview_target(self.page, preview.pages.len());
            if target > preview.loaded {
                let mut preview = preview.clone();
                let (tx, rx) = mpsc::channel();
                self.preview_rx = Some(rx);
                thread::spawn(move || {
                    let result = render_preview_range(&preview, preview.loaded, target).map(|()| {
                        preview.loaded = target;
                        preview
                    });
                    let _ = tx.send(result);
                });
            }
        }
        if let Some(result) = self.print_rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.print_rx = None;
            match result {
                Ok(()) => self.message = "打印任务已提交。".into(),
                Err(error) => self.error = Some(error),
            }
        }
        if self.prepared.is_none() || self.preview_rx.is_some() || self.print_rx.is_some() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    pub fn show(&mut self, ctx: &Context) -> bool {
        self.poll(ctx);
        let mut open = true;
        let mut print = false;
        let dialog_size = (ctx.content_rect().size() - Vec2::new(48.0, 96.0))
            .min(Vec2::new(1040.0, 760.0))
            .max(Vec2::new(280.0, 220.0));
        egui::Window::new(egui::RichText::new("打印").size(16.0))
            .id(egui::Id::unique("print_dialog"))
            .open(&mut open)
            .collapsible(false)
            .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(dialog_size)
            .show(ctx, |ui| {
                let size = ui.available_size();
                let settings_width = (size.x * 0.38).clamp(220.0, 330.0).min(size.x * 0.55);
                let preview_width =
                    (size.x - settings_width - ui.spacing().item_spacing.x * 2.0 - 6.0).max(80.0);
                ui.set_min_size(size);
                ui.horizontal_top(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(settings_width, size.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(settings_width);
                            egui::ScrollArea::vertical()
                                .id_salt("print_settings_scroll")
                                .max_height(size.y)
                                .auto_shrink([false, false])
                                .show(ui, |ui| {
                                    ui.set_width(settings_width);
                                    ui.add(egui::Label::new(&self.title).truncate())
                                        .on_hover_text(&self.title);
                                    let control_width = (settings_width - 90.0).max(80.0);
                                    egui::Grid::new("print_controls").num_columns(2).show(
                                        ui,
                                        |ui| {
                                            ui.label("打印机");
                                            egui::ComboBox::from_id_salt("printer")
                                                .width(control_width)
                                                .truncate()
                                                .selected_text(
                                                    if self.settings.printer.is_empty() {
                                                        "未安装打印机"
                                                    } else {
                                                        &self.settings.printer
                                                    },
                                                )
                                                .show_ui(ui, |ui| {
                                                    if let Some(prepared) = &self.prepared {
                                                        for printer in &prepared.printers {
                                                            ui.selectable_value(
                                                                &mut self.settings.printer,
                                                                printer.clone(),
                                                                printer,
                                                            );
                                                        }
                                                    }
                                                });
                                            ui.end_row();
                                            ui.label("纸张");
                                            egui::ComboBox::from_id_salt("paper")
                                                .width(control_width)
                                                .truncate()
                                                .selected_text(&self.settings.paper)
                                                .show_ui(ui, |ui| {
                                                    for paper in ["A4", "A5", "Letter", "Legal"] {
                                                        ui.selectable_value(
                                                            &mut self.settings.paper,
                                                            paper.into(),
                                                            paper,
                                                        );
                                                    }
                                                });
                                            ui.end_row();
                                            ui.label("方向");
                                            ui.horizontal(|ui| {
                                                ui.selectable_value(
                                                    &mut self.settings.landscape,
                                                    false,
                                                    "纵向",
                                                );
                                                ui.selectable_value(
                                                    &mut self.settings.landscape,
                                                    true,
                                                    "横向",
                                                );
                                            });
                                            ui.end_row();
                                            ui.label("份数");
                                            ui.add(
                                                egui::DragValue::new(&mut self.settings.copies)
                                                    .range(1..=99),
                                            );
                                            ui.end_row();
                                            ui.label("页边距");
                                            ui.add(
                                                egui::DragValue::new(&mut self.settings.margin_mm)
                                                    .range(5.0..=30.0)
                                                    .suffix(" mm"),
                                            );
                                            ui.end_row();
                                            ui.label("颜色");
                                            ui.checkbox(&mut self.settings.color, "彩色");
                                            ui.end_row();
                                            ui.label("双面");
                                            egui::ComboBox::from_id_salt("duplex")
                                                .width(control_width)
                                                .truncate()
                                                .selected_text(
                                                    ["单面", "长边翻页", "短边翻页"]
                                                        [self.settings.duplex as usize],
                                                )
                                                .show_ui(ui, |ui| {
                                                    for (value, name) in [
                                                        (0, "单面"),
                                                        (1, "长边翻页"),
                                                        (2, "短边翻页"),
                                                    ] {
                                                        ui.selectable_value(
                                                            &mut self.settings.duplex,
                                                            value,
                                                            name,
                                                        );
                                                    }
                                                });
                                            ui.end_row();
                                            ui.label("页码范围");
                                            ui.add(
                                                egui::TextEdit::singleline(
                                                    &mut self.settings.range,
                                                )
                                                .hint_text("全部 / 1-3,5")
                                                .desired_width(control_width),
                                            );
                                            ui.end_row();
                                        },
                                    );
                                    ui.separator();
                                    ui.label("打印内容");
                                    ui.checkbox(&mut self.settings.highlights, "高亮");
                                    ui.checkbox(&mut self.settings.notes, "笔记");
                                    ui.add_enabled_ui(self.settings.notes, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.label("首页笔记栏");
                                            ui.selectable_value(
                                                &mut self.settings.first_note_left,
                                                true,
                                                "左侧",
                                            );
                                            ui.selectable_value(
                                                &mut self.settings.first_note_left,
                                                false,
                                                "右侧",
                                            );
                                        });
                                        if self.settings.duplex != 0 {
                                            ui.small("后续输出页的笔记栏左右交替。");
                                        }
                                    });
                                    ui.separator();
                                    let ready = self.preview.as_ref().is_some_and(|preview| {
                                        preview.settings == self.settings
                                            && !preview.pages.is_empty()
                                    });
                                    if ui
                                        .add_enabled(
                                            ready
                                                && self.preview_rx.is_none()
                                                && self.print_rx.is_none()
                                                && !self.settings.printer.is_empty(),
                                            egui::Button::new("打印"),
                                        )
                                        .clicked()
                                    {
                                        print = true;
                                    }
                                    if self.prepared.is_none() {
                                        ui.spinner();
                                        ui.label("正在准备文档…");
                                    } else if self.preview_rx.is_some() {
                                        ui.spinner();
                                        ui.label("正在更新打印预览…");
                                    }
                                    if !self.message.is_empty() {
                                        ui.label(&self.message);
                                    }
                                    if let Some(error) = &self.error {
                                        ui.colored_label(egui::Color32::RED, error);
                                    }
                                });
                        },
                    );
                    ui.separator();
                    ui.allocate_ui_with_layout(
                        Vec2::new(preview_width, size.y),
                        egui::Layout::top_down(egui::Align::Min),
                        |ui| {
                            ui.set_width(preview_width);
                            ui.horizontal(|ui| {
                                if ui.button("上一页").clicked() {
                                    self.page = self.page.saturating_sub(1);
                                }
                                let count = self
                                    .preview
                                    .as_ref()
                                    .map_or(0, |preview| preview.pages.len());
                                ui.label(format!(
                                    "{} / {}",
                                    if count == 0 { 0 } else { self.page + 1 },
                                    count
                                ));
                                if ui.button("下一页").clicked() {
                                    self.page = (self.page + 1).min(count.saturating_sub(1));
                                }
                            });
                            if let Some(path) = self
                                .preview
                                .as_ref()
                                .and_then(|preview| preview.pages.get(self.page))
                            {
                                if self
                                    .texture
                                    .as_ref()
                                    .is_some_and(|(loaded, _)| loaded != path)
                                {
                                    self.texture = None;
                                }
                                if self.texture.is_none()
                                    && let Ok(image) = image::open(path)
                                {
                                    let rgba = image.into_rgba8();
                                    let texture = ctx.load_texture(
                                        "print_preview",
                                        ColorImage::from_rgba_unmultiplied(
                                            [rgba.width() as usize, rgba.height() as usize],
                                            rgba.as_raw(),
                                        ),
                                        TextureOptions::LINEAR,
                                    );
                                    self.texture = Some((path.clone(), texture));
                                }
                                if let Some((_, texture)) = &self.texture {
                                    let size = texture.size_vec2();
                                    let available = ui.available_size().max(Vec2::splat(1.0));
                                    let scale =
                                        (available.x / size.x).min(available.y / size.y).min(1.0);
                                    ui.image((texture.id(), size * scale));
                                } else {
                                    ui.spinner();
                                    ui.label("正在生成当前页及后续预览…");
                                }
                            }
                        },
                    );
                });
            });
        if print && let Some(preview) = &self.preview {
            match selected_pages(&self.settings.range, preview.pages.len()) {
                Err(error) => self.error = Some(error),
                Ok(_) => {
                    let mut settings = self.settings.clone();
                    if settings
                        .printer
                        .to_ascii_lowercase()
                        .contains("print to pdf")
                    {
                        if let Some(path) = rfd::FileDialog::new()
                            .add_filter("PDF", &["pdf"])
                            .set_file_name("document.pdf")
                            .save_file()
                        {
                            settings.output = path.to_string_lossy().into_owned();
                        } else {
                            return open;
                        }
                    }
                    if let Some(directory) = preview
                        .pages
                        .first()
                        .and_then(|path| path.parent())
                        .map(Path::to_owned)
                    {
                        let prepared = Arc::clone(&preview._prepared);
                        let preview = preview.clone();
                        let (tx, rx) = mpsc::channel();
                        self.print_rx = Some(rx);
                        thread::spawn(move || {
                            let path = directory.join("print-settings.json");
                            let result = (|| {
                                crate::storage::write_json(&path, &settings)
                                    .map_err(|error| error.to_string())?;
                                let layout = preview.layout.as_ref().ok_or("缺少打印布局")?;
                                let _guard = layout
                                    .render_lock
                                    .lock()
                                    .map_err(|error| error.to_string())?;
                                let pages = selected_pages(&settings.range, preview.pages.len())?;
                                let mut missing = pages
                                    .into_iter()
                                    .filter(|&index| !preview.pages[index].exists())
                                    .collect::<Vec<_>>();
                                missing.sort_unstable();
                                let mut first = 0;
                                while first < missing.len() {
                                    let mut last = first + 1;
                                    while last < missing.len()
                                        && missing[last] == missing[last - 1] + 1
                                    {
                                        last += 1;
                                    }
                                    render_preview_range_unlocked(
                                        &preview,
                                        missing[first],
                                        missing[last - 1] + 1,
                                    )?;
                                    first = last;
                                }
                                run_engine(&prepared, "print", &path, &directory)
                            })();
                            let _ = tx.send(result);
                        });
                    }
                }
            }
        }
        open
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn wait_for_pdf_pages(path: &Path, expected: usize) {
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        loop {
            if let Ok(pdf) = crate::pdf::PdfDoc::open(path)
                && pdf.page_count() == expected
            {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "virtual printer did not finish writing a readable {expected}-page PDF"
            );
            thread::sleep(Duration::from_millis(100));
        }
    }

    #[test]
    fn long_cjk_epub_does_not_change_the_first_paragraph_when_font_atlas_rebuilds() {
        use crate::flow::{Block, Inline};
        let text = "中国古典文学阅读验证：汉字、标点与假名不应在排版后变成其他字符。";
        let baseline = crate::flow::parse_markdown_document(text, None);
        let mut document = baseline.clone();
        document.kind = "epub";
        let characters = (0x4e00..0x6e00)
            .filter_map(char::from_u32)
            .collect::<Vec<_>>();
        for chunk in characters.chunks(80) {
            document.chapters[0]
                .blocks
                .push(Block::Paragraph(vec![Inline::Text(chunk.iter().collect())]));
        }
        document.chapters[0]
            .blocks
            .push(Block::Paragraph(vec![Inline::Text(text.into())]));
        let (blocks, textures) = crate::app::print_flow_scenes(&baseline, false, 700.0, 19.0);
        let crop_height = blocks[0].height.ceil() as u32;
        let expected = rich::RichLayout::paginate(blocks, textures, 700, 1000, &[Vec::new()])
            .render(0)
            .unwrap();
        let (blocks, textures) = crate::app::print_flow_scenes(&document, false, 700.0, 19.0);
        let actual = rich::RichLayout::paginate(blocks, textures, 700, 1000, &[Vec::new()])
            .render(0)
            .unwrap();
        assert!(
            image::imageops::crop_imm(&actual, 0, 0, 700, crop_height).to_image()
                == image::imageops::crop_imm(&expected, 0, 0, 700, crop_height).to_image(),
            "laying out later CJK chapters must not overwrite the first paragraph's glyphs"
        );
    }
    fn png_count(path: &Path, prefix: &str) -> usize {
        fs::read_dir(path)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                let name = entry.file_name().to_string_lossy().into_owned();
                name.starts_with(prefix) && name.ends_with(".png")
            })
            .count()
    }

    #[test]
    fn rich_flow_reflows_notes_and_renders_images_math_ruby_and_markdown_styles() {
        use crate::flow::{Block, Inline};
        let mut document = crate::flow::parse_markdown_document(
            "# Print heading\n\nNormal **bold** and *italic*, ~~strike~~, `code`, [link](https://example.org).\n\nInline $x^2+1$ formula.\n\n$$\\frac{a}{b}=\\sqrt{x}$$\n\n- First list item\n- Second list item\n\n| Name | Value |\n| --- | --- |\n| Alpha | 12 |\n\n```rust\nlet value = 42;\n```",
            None,
        );
        let (blocks, _, _) = crate::flow::parse_xhtml_blocks_with_anchors(
            "<html><body><p><ruby>漢字<rt>かんじ</rt></ruby> and EPUB <b>strong</b> <i>italic</i></p><p><math><mfrac><mi>a</mi><mi>b</mi></mfrac></math></p></body></html>",
            "chapter.xhtml",
            0,
            &std::collections::HashMap::new(),
        );
        document.chapters[0].blocks.extend(blocks);
        let mut encoded = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            160,
            80,
            image::Rgba([230, 20, 35, 255]),
        ))
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
        document
            .resources
            .insert("picture.png".into(), encoded.into_inner());
        document.chapters[0].blocks.push(Block::Image {
            path: "picture.png".into(),
            alt: "illustration".into(),
        });
        assert!(document.chapters[0].blocks.iter().any(|block|matches!(block,Block::Paragraph(inlines) if inlines.iter().any(|inline|matches!(inline,Inline::Ruby{..})))));
        let prepared = prepare(
            Input::Flow(Arc::new(document), false),
            "rich".into(),
            Vec::new(),
            19.0,
        )
        .unwrap();
        let normal_settings = PrintSettings::default();
        let normal_dir = prepared.dir.path().join("normal");
        fs::create_dir(&normal_dir).unwrap();
        let normal = build_layout(&prepared, &normal_settings, &normal_dir).unwrap();
        let note_settings = PrintSettings {
            notes: true,
            ..Default::default()
        };
        let note_dir = prepared.dir.path().join("notes");
        fs::create_dir(&note_dir).unwrap();
        let narrow = build_layout(&prepared, &note_settings, &note_dir).unwrap();
        let (LayoutKind::Flow(wide), LayoutKind::Flow(narrow)) = (&normal.kind, &narrow.kind)
        else {
            panic!("flow layout expected")
        };
        assert!(narrow.width < wide.width);
        assert_eq!(narrow.height, wide.height);
        let first_height = |rich: &rich::RichLayout| {
            rich.blocks
                .iter()
                .flat_map(|block| &block.glyphs)
                .next()
                .unwrap()
                .2
                .height()
        };
        assert!(
            (first_height(wide) - first_height(narrow)).abs() < 0.1,
            "notes must rewrap, not shrink fonts"
        );
        assert!(
            wide.textures.len() >= 4,
            "font, formula and illustration textures must be retained"
        );
        let mut red = 0;
        let mut ink = 0;
        for page in 0..wide.pages.len() {
            let bitmap = wide.render(page).unwrap();
            red += bitmap
                .pixels()
                .filter(|pixel| pixel[0] > 180 && pixel[1] < 80 && pixel[2] < 100)
                .count();
            ink += bitmap
                .pixels()
                .filter(|pixel| pixel[0] < 150 && pixel[1] < 150 && pixel[2] < 150)
                .count();
            if page == 0 {
                bitmap.save("target/print-rich-verification.png").unwrap();
            }
        }
        assert!(red > 10000, "embedded image must appear in print pixels");
        assert!(
            ink > 5000,
            "text, formula and ruby must produce visible ink"
        );
    }

    #[test]
    fn preview_creates_only_first_five_pages_then_the_requested_lookahead() {
        let document = crate::flow::parse_markdown_document(
            &"A long printable paragraph containing reflowable text.\n\n".repeat(400),
            None,
        );
        let prepared = prepare(
            Input::Flow(Arc::new(document), false),
            "lazy".into(),
            Vec::new(),
            19.0,
        )
        .unwrap();
        let output = prepared.dir.path().join("lazy");
        let mut preview = create_preview(
            Arc::clone(&prepared),
            PrintSettings::default(),
            output.clone(),
        )
        .unwrap();
        assert!(preview.pages.len() > 10);
        assert_eq!(preview.loaded, 5);
        assert_eq!(png_count(&output, "page-"), 5);
        assert_eq!(png_count(&output, "source-"), 5);
        let target = preview_target(4, preview.pages.len());
        assert_eq!(target, 10);
        render_preview_range(&preview, preview.loaded, target).unwrap();
        preview.loaded = target;
        assert_eq!(png_count(&output, "page-"), 10);
        assert_eq!(png_count(&output, "source-"), 10);
        assert!(!preview.pages[10].exists());
        assert_eq!(preview_target(5, preview.pages.len()), 11);
    }

    fn pdf_fixture(path: &Path, count: usize) {
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!(
                "<< /Type /Pages /Count {count} /Kids [{}] >>",
                (0..count)
                    .map(|index| format!("{} 0 R", 4 + index * 2))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        for index in 0..count {
            objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 595 842] /Resources << /Font << /F1 3 0 R >> >> /Contents {} 0 R >>",5+index*2));
            let text = format!("BT /F1 18 Tf 72 700 Td (Preview page {}) Tj ET", index + 1);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{text}\nendstream",
                text.len()
            ));
        }
        let mut pdf = String::from("%PDF-1.4\n");
        let mut offsets = vec![0];
        for (index, object) in objects.iter().enumerate() {
            offsets.push(pdf.len());
            pdf.push_str(&format!("{} 0 obj\n{object}\nendobj\n", index + 1));
        }
        let xref = pdf.len();
        pdf.push_str(&format!(
            "xref\n0 {}\n0000000000 65535 f \n",
            objects.len() + 1
        ));
        for offset in offsets.iter().skip(1) {
            pdf.push_str(&format!("{offset:010} 00000 n \n"));
        }
        pdf.push_str(&format!(
            "trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        fs::write(path, pdf).unwrap();
    }

    #[test]
    fn pdf_keeps_page_geometry_and_does_not_rasterize_pages_beyond_preview_batch() {
        let fixture = tempfile::tempdir().unwrap();
        let pdf = fixture.path().join("eight-pages.pdf");
        pdf_fixture(&pdf, 8);
        let prepared = prepare(Input::Pdf(pdf), "PDF".into(), Vec::new(), 19.0).unwrap();
        assert_eq!(png_count(prepared.dir.path(), "source-"), 0);
        let output = prepared.dir.path().join("pdf-notes");
        let settings = PrintSettings {
            notes: true,
            ..Default::default()
        };
        let preview =
            create_preview(Arc::clone(&prepared), settings.clone(), output.clone()).unwrap();
        assert_eq!(preview.pages.len(), 8);
        assert_eq!(png_count(&output, "source-"), 5);
        assert_eq!(png_count(&output, "page-"), 5);
        let layout = preview.layout.as_ref().unwrap();
        assert!(!layout.source.reflow);
        assert_eq!(
            (layout.source.pages[0].width, layout.source.pages[0].height),
            (595.0, 842.0)
        );
        let metadata: serde_json::Value =
            crate::storage::read_json(&output.join("layout.json")).unwrap();
        assert_eq!(metadata[0]["reflow"], false);
        assert_eq!(metadata[0]["body_width"], page_dimensions(&settings).0);
        if prepared
            .printers
            .iter()
            .any(|printer| printer == "Microsoft Print to PDF")
        {
            let printed = output.join("unpreviewed-pages.pdf");
            let print_settings = PrintSettings {
                printer: "Microsoft Print to PDF".into(),
                range: "6-8".into(),
                output: printed.to_string_lossy().into_owned(),
                ..settings
            };
            let settings_file = output.join("print-settings.json");
            crate::storage::write_json(&settings_file, &print_settings).unwrap();
            render_preview_range(&preview, 5, 8).unwrap();
            run_engine(&prepared, "print", &settings_file, &output).unwrap();
            wait_for_pdf_pages(&printed, 3);
        }
    }
    #[test]
    fn word_reflows_to_note_column_width_without_shrinking_the_document_font() {
        use std::io::Write;
        let fixture = tempfile::tempdir().unwrap();
        let path = fixture.path().join("reflow.docx");
        let mut zip = zip::ZipWriter::new(fs::File::create(&path).unwrap());
        let options = zip::write::SimpleFileOptions::default();
        for (name, contents) in [
            (
                "[Content_Types].xml",
                r#"<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#,
            ),
            (
                "_rels/.rels",
                r#"<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#,
            ),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(contents.as_bytes()).unwrap();
        }
        let paragraphs=(0..100).map(|index|format!("<w:p><w:r><w:rPr><w:sz w:val=\"24\"/></w:rPr><w:t>Paragraph {index:03}: Word text is rewrapped at its original font size when a notes column is enabled. Additional text verifies that the narrower column creates more lines without scaling the type. The next sentence keeps pagination measurable at different page margins.</w:t></w:r></w:p>")).collect::<String>();
        zip.start_file("word/document.xml", options).unwrap();
        write!(zip,"<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{paragraphs}<w:sectPr/></w:body></w:document>").unwrap();
        zip.finish().unwrap();
        let prepared = prepare(Input::Word(path.clone()), "Word".into(), Vec::new(), 19.0).unwrap();
        let wide_dir = prepared.dir.path().join("word-wide");
        fs::create_dir(&wide_dir).unwrap();
        let narrow_dir = prepared.dir.path().join("word-narrow");
        fs::create_dir(&narrow_dir).unwrap();
        let wide_settings = PrintSettings::default();
        let narrow_settings = PrintSettings {
            notes: true,
            ..Default::default()
        };
        let wide = build_layout(&prepared, &wide_settings, &wide_dir).unwrap();
        let narrow = build_layout(&prepared, &narrow_settings, &narrow_dir).unwrap();
        assert!(wide.source.reflow && narrow.source.reflow);
        assert!(narrow.source.pages[0].width < wide.source.pages[0].width);
        assert!(narrow.source.pages.len() > wide.source.pages.len());
        let (LayoutKind::Word(_, wide_size), LayoutKind::Word(_, narrow_size)) =
            (&wide.kind, &narrow.kind)
        else {
            panic!("Word layouts expected")
        };
        let wide_word = crate::word::Word::open_for_print(&path, *wide_size).unwrap();
        let narrow_word = crate::word::Word::open_for_print(&path, *narrow_size).unwrap();
        assert_eq!(
            wide_word.text_runs(0).unwrap()[0].size,
            narrow_word.text_runs(0).unwrap()[0].size
        );
        assert_eq!(png_count(&wide_dir, "source-"), 0);
        assert_eq!(png_count(&narrow_dir, "source-"), 0);
    }

    #[test]
    fn print_window_reserves_height_for_controls_and_preview_with_long_printer_names() {
        fn inspect(
            shape: &egui::Shape,
            clip: egui::Rect,
            texture: egui::TextureId,
            labels: &mut Vec<String>,
            images: &mut Vec<egui::Rect>,
        ) {
            match shape {
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        inspect(shape, clip, texture, labels, images);
                    }
                }
                egui::Shape::Text(text) => {
                    let bounds = egui::Rect::from_min_size(text.pos, text.galley.size());
                    if clip.contains_rect(bounds) {
                        labels.push(text.galley.job.text.clone());
                    }
                }
                egui::Shape::Rect(rect) if rect.fill_texture_id() == texture => {
                    images.push(rect.rect.intersect(clip));
                }
                egui::Shape::Mesh(mesh) if mesh.texture_id == texture => {
                    images.push(mesh.calc_bounds().intersect(clip))
                }
                _ => {}
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let page = dir.path().join("sample.png");
        image::RgbaImage::from_pixel(827, 1169, image::Rgba([255, 255, 255, 255]))
            .save(&page)
            .unwrap();
        let printer = "MFP-M233sdw (HP LaserJet MFP M233sdw) - Long Printer Name".to_owned();
        let prepared = Arc::new(Prepared {
            dir,
            printers: vec![printer.clone()],
            title: "fixture".into(),
            font_size: 19.0,
            annotations: Vec::new(),
            input: Input::Flow(
                Arc::new(crate::flow::parse_markdown_document("fixture", None)),
                false,
            ),
        });
        let settings = PrintSettings {
            printer,
            ..Default::default()
        };
        let (_, source_rx) = mpsc::channel();
        let mut dialog = PrintDialog {
            title: "很长的文档标题".repeat(20),
            settings: settings.clone(),
            source_rx,
            prepared: Some(Arc::clone(&prepared)),
            preview_rx: None,
            preview: Some(Preview {
                pages: vec![page],
                settings: settings.clone(),
                _prepared: prepared,
                layout: None,
                sheet_sources: vec![0],
                loaded: 1,
            }),
            requested: Some(settings),
            page: 0,
            texture: None,
            error: None,
            print_rx: None,
            message: String::new(),
        };
        for scale in [0.5, 1.0, 1.5, 2.0] {
            let ctx = Context::default();
            ctx.set_zoom_factor(scale);
            for theme in [egui::Theme::Light, egui::Theme::Dark] {
                ctx.style_mut_of(theme, |style| {
                    for font in style.text_styles.values_mut() {
                        font.size = 18.0;
                    }
                    style.spacing.interact_size.y = 28.0;
                    style.spacing.item_spacing.y = 7.0;
                });
            }
            for _ in 0..4 {
                let frame = ctx.run_ui(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            Vec2::new(1200.0, 900.0) / scale,
                        )),
                        ..Default::default()
                    },
                    |ui| {
                        dialog.show(ui.ctx());
                    },
                );
                frame.drop_without_applying_deltas();
            }
            let frame = ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        Vec2::new(1200.0, 900.0) / scale,
                    )),
                    ..Default::default()
                },
                |ui| {
                    dialog.show(ui.ctx());
                },
            );
            let texture = dialog.texture.as_ref().unwrap().1.id();
            let mut labels = Vec::new();
            let mut images = Vec::new();
            for shape in &frame.shapes {
                inspect(
                    &shape.shape,
                    shape.clip_rect,
                    texture,
                    &mut labels,
                    &mut images,
                );
            }
            for label in ["纸张", "方向", "份数"] {
                assert!(
                    labels.iter().any(|text| text == label),
                    "{label} clipped at {scale}x: {labels:?}"
                );
            }
            if scale <= 1.0 {
                assert!(
                    labels.iter().any(|text| text == "打印内容"),
                    "print-content options must be visible"
                );
            }
            assert!(
                images.iter().any(|rect| rect.height() >= 200.0),
                "preview collapsed at {scale}x: {images:?}"
            );
            frame.drop_without_applying_deltas();
        }
    }
    #[test]
    fn long_notes_continue_without_dropping_text_and_virtual_pdf_prints() {
        let document = crate::flow::parse_markdown_document("目标句子和正文。", None);
        let note = "长笔记内容，需要续页时也必须完整保留。".repeat(100);
        let annotations = vec![Annotation {
            kind: "note".into(),
            text: note.clone(),
            color: "yellow".into(),
            parts: vec![crate::storage::Anchor {
                quote: "目标句子".into(),
                ..Default::default()
            }],
            ..Default::default()
        }];
        let prepared = prepare(
            Input::Flow(Arc::new(document), false),
            "长笔记测试".into(),
            annotations,
            19.0,
        )
        .unwrap();
        let mut settings = PrintSettings {
            notes: true,
            duplex: 1,
            first_note_left: true,
            ..Default::default()
        };
        let output = prepared.dir.path().join("long-note");
        fs::create_dir(&output).unwrap();
        let settings_path = output.join("settings.json");
        crate::storage::write_json(&settings_path, &settings).unwrap();
        let _preview =
            create_preview(Arc::clone(&prepared), settings.clone(), output.clone()).unwrap();
        let layout: Vec<serde_json::Value> =
            crate::storage::read_json(&output.join("layout.json")).unwrap();
        assert!(
            layout.len() > 1,
            "a long note must continue on additional sheets"
        );
        let mut printed_characters = 0;
        for (index, sheet) in layout.iter().enumerate() {
            assert_eq!(sheet["note_left"], index % 2 == 0);
            printed_characters += sheet["note_characters"].as_u64().unwrap();
        }
        assert_eq!(printed_characters, note.chars().count() as u64);
        if prepared
            .printers
            .iter()
            .any(|printer| printer == "Microsoft Print to PDF")
        {
            settings.printer = "Microsoft Print to PDF".into();
            settings.duplex = 0;
            let pdf = output.join("virtual-printer-output.pdf");
            settings.output = pdf.to_string_lossy().into_owned();
            settings.range = "1".into();
            crate::storage::write_json(&settings_path, &settings).unwrap();
            let _preview =
                create_preview(Arc::clone(&prepared), settings.clone(), output.clone()).unwrap();
            run_engine(&prepared, "print", &settings_path, &output).unwrap();
            wait_for_pdf_pages(&pdf, 1);
            let bytes = fs::read(&pdf).unwrap();
            assert!(bytes.starts_with(b"%PDF-"));
            assert!(bytes.len() > 1000);
        }
    }
    #[test]
    fn windows_preview_tracks_highlights_notes_and_duplex_sides() {
        let mut document = crate::flow::parse_markdown_document(
            &"目标句子和正文。\n\n第二段正文。\n\n".repeat(40),
            None,
        );
        document.chapters.push(document.chapters[0].clone());
        let annotations = (0..2)
            .map(|index| Annotation {
                kind: "note".into(),
                color: "yellow".into(),
                text: "这是一条打印到边栏的笔记。".into(),
                parts: vec![crate::storage::Anchor {
                    index,
                    quote: "目标句子".into(),
                    ..Default::default()
                }],
                ..Default::default()
            })
            .collect();
        let prepared = prepare(
            Input::Flow(Arc::new(document), false),
            "打印验证".into(),
            annotations,
            19.0,
        )
        .unwrap();
        let mut settings = PrintSettings {
            highlights: false,
            ..Default::default()
        };
        let mut images = Vec::new();
        for step in 0..4 {
            if step == 1 {
                settings.highlights = true;
            }
            if step == 2 {
                settings.notes = true;
                settings.duplex = 1;
            }
            if step == 3 {
                settings.first_note_left = true;
            }
            let output = prepared.dir.path().join(format!("test-{step}"));
            fs::create_dir(&output).unwrap();
            let path = output.join("settings.json");
            crate::storage::write_json(&path, &settings).unwrap();
            let _preview =
                create_preview(Arc::clone(&prepared), settings.clone(), output.clone()).unwrap();
            let pages: Vec<String> = crate::storage::read_json(&output.join("pages.json")).unwrap();
            assert!(pages.len() >= 2);
            images.push(fs::read(output.join(&pages[0])).unwrap());
            if step >= 2 {
                let layout: serde_json::Value =
                    crate::storage::read_json(&output.join("layout.json")).unwrap();
                assert_eq!(layout[0]["note_left"], step == 3);
                assert_eq!(layout[1]["note_left"], step != 3);
                assert_eq!(layout[0]["notes"], 1);
            }
        }
        assert_ne!(
            images[0], images[1],
            "highlight checkbox must change the actual preview"
        );
        assert_ne!(
            images[1], images[2],
            "notes checkbox must add the margin and scale the content"
        );
        assert_ne!(
            images[2], images[3],
            "first-page side must change the page layout"
        );
    }
    #[test]
    fn page_ranges_validate_and_keep_output_order() {
        assert_eq!(selected_pages("1-3,5,3", 5).unwrap(), vec![0, 1, 2, 4]);
        assert_eq!(selected_pages("", 2).unwrap(), vec![0, 1]);
        for range in ["0", "6", "3-1", "a", "1-2-3"] {
            assert!(selected_pages(range, 5).is_err());
        }
    }
    #[test]
    fn unicode_annotation_offsets_are_converted_for_windows_text_layout() {
        let annotation = Annotation {
            kind: "note".into(),
            text: "说明".into(),
            parts: vec![crate::storage::Anchor {
                start: 2,
                quote: "正文".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let marks = marks_for("😀前正文", 0, &[annotation], false);
        assert_eq!(marks[0].start, 3);
        assert_eq!(marks[0].end, 5);
    }
}
