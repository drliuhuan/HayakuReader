use docboss_core::{Block, Document, Error, FontEntry, FontSlots, Inline, RunContent};
use docboss_font::FontDatabase;
use docboss_layout::{GlyphRun, Item, Layout, LayoutOptions, Page};
use docboss_render::Format;
use serde::Serialize;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

#[derive(Clone, Serialize)]
pub struct Metadata {
    pub pages: Vec<[f32; 2]>,
    pub images: Vec<Vec<[f32; 4]>>,
    pub outline: Vec<Heading>,
}

#[derive(Clone, Serialize)]
pub struct Heading {
    pub title: String,
    pub index: usize,
}

#[derive(Serialize)]
pub struct TextRun<'a> {
    text: &'a str,
    left: f32,
    top: f32,
    width: f32,
    size: f32,
    line_break: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

pub struct Word {
    pub metadata: Metadata,
    layout: Layout,
    block_links: Vec<Vec<(String, String)>>,
}

struct CjkFontSupport {
    default_family: String,
    default_regular: Option<Arc<[u8]>>,
    default_bold: Option<Arc<[u8]>>,
    alias_entries: Vec<FontEntry>,
    known_families: HashSet<String>,
}

fn normalize_font_name(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn font_search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("WINDIR") {
            dirs.push(PathBuf::from(root).join("Fonts"));
        } else {
            dirs.push(PathBuf::from(r"C:\Windows\Fonts"));
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(PathBuf::from(root).join(r"Microsoft\Windows\Fonts"));
        }
    }
    #[cfg(not(windows))]
    {
        dirs.push(PathBuf::from("/usr/share/fonts"));
        dirs.push(PathBuf::from("/usr/local/share/fonts"));
    }
    dirs
}

fn load_font_file(dirs: &[PathBuf], filenames: &[&str]) -> Option<Arc<[u8]>> {
    for dir in dirs {
        for filename in filenames {
            let candidate = dir.join(filename);
            if let Ok(bytes) = std::fs::read(&candidate) {
                return Some(Arc::from(bytes));
            }
        }
    }
    None
}

fn cjk_font_support() -> &'static CjkFontSupport {
    static SUPPORT: OnceLock<CjkFontSupport> = OnceLock::new();
    SUPPORT.get_or_init(|| {
        let dirs = font_search_dirs();
        let deng_reg = load_font_file(&dirs, &["Deng.ttf", "deng.ttf"]);
        let deng_bold = load_font_file(&dirs, &["Dengb.ttf", "dengb.ttf"]);
        let deng_light = load_font_file(&dirs, &["Dengl.ttf", "dengl.ttf"]);
        let msyh_reg = load_font_file(&dirs, &["msyh.ttc"]);
        let msyh_bold = load_font_file(&dirs, &["msyhbd.ttc"]);
        let simsun_reg = load_font_file(&dirs, &["simsun.ttc"]);
        let simhei_reg = load_font_file(&dirs, &["simhei.ttf"]);
        let simkai_reg = load_font_file(&dirs, &["simkai.ttf"]);
        let simfang_reg = load_font_file(&dirs, &["simfang.ttf"]);

        let (default_family, default_regular, default_bold) = if let Some(reg) = deng_reg.clone() {
            ("DengXian".to_owned(), Some(reg), deng_bold.clone())
        } else if let Some(reg) = msyh_reg.clone() {
            ("Microsoft YaHei".to_owned(), Some(reg), msyh_bold.clone())
        } else if let Some(reg) = simsun_reg.clone() {
            ("SimSun".to_owned(), Some(reg), None)
        } else if let Some(reg) = simhei_reg.clone() {
            ("SimHei".to_owned(), Some(reg), None)
        } else {
            ("Times New Roman".to_owned(), None, None)
        };

        let specs = [
            ("等线", "DengXian", deng_reg.as_ref(), deng_bold.as_ref()),
            ("等线 Light", "DengXian Light", deng_light.as_ref(), None),
            ("微软雅黑", "Microsoft YaHei", msyh_reg.as_ref(), msyh_bold.as_ref()),
            ("宋体", "SimSun", simsun_reg.as_ref(), None),
            ("新宋体", "NSimSun", simsun_reg.as_ref(), None),
            ("黑体", "SimHei", simhei_reg.as_ref(), None),
            ("楷体", "KaiTi", simkai_reg.as_ref(), None),
            ("楷体_GB2312", "KaiTi_GB2312", simkai_reg.as_ref(), None),
            ("仿宋", "FangSong", simfang_reg.as_ref(), None),
            ("仿宋_GB2312", "FangSong_GB2312", simfang_reg.as_ref(), None),
        ];

        let mut alias_entries = Vec::new();
        for (zh, en, reg, bold) in specs {
            let Some(reg_data) = reg else {
                continue;
            };
            alias_entries.push(FontEntry {
                name: zh.to_owned(),
                alt_name: Some(en.to_owned()),
                family: None,
                pitch: None,
                embedded_regular: Some(reg_data.clone()),
                embedded_bold: bold.cloned(),
                embedded_italic: None,
                embedded_bold_italic: None,
            });
        }

        let system_db = FontDatabase::system();
        let mut known_families: HashSet<String> = system_db
            .faces()
            .iter()
            .map(|face| normalize_font_name(&face.family))
            .filter(|name| !name.is_empty())
            .collect();
        for entry in &alias_entries {
            known_families.insert(normalize_font_name(&entry.name));
            if let Some(alt) = entry.alt_name.as_deref() {
                known_families.insert(normalize_font_name(alt));
            }
        }
        for builtin in [
            "calibri",
            "calibrilight",
            "cambria",
            "cambriamath",
            "arial",
            "helvetica",
            "helveticaneue",
            "arialmt",
            "timesnewroman",
            "times",
            "timesnewromanpsmt",
            "couriernew",
            "courier",
            "georgia",
            "verdana",
            "tahoma",
            "consolas",
            "lucidaconsole",
            "symbol",
            "segoeui",
            "aptos",
            "aptosdisplay",
            "calibribody",
            "msmincho",
            "mincho",
            "mingliu",
            "msgothic",
            "yugothic",
            "meiryo",
        ] {
            known_families.insert(builtin.to_owned());
        }

        CjkFontSupport {
            default_family,
            default_regular,
            default_bold,
            alias_entries,
            known_families,
        }
    })
}

fn push_slot_fonts(slots: &FontSlots, out: &mut Vec<String>) {
    for candidate in [
        slots.ascii.as_deref(),
        slots.high_ansi.as_deref(),
        slots.east_asia.as_deref(),
        slots.complex.as_deref(),
    ]
    .into_iter()
    .flatten()
    {
        let trimmed = candidate.trim();
        if !trimmed.is_empty() {
            out.push(trimmed.to_owned());
        }
    }
}

fn collect_inline_fonts(inlines: &[Inline], out: &mut Vec<String>) {
    for inline in inlines {
        match inline {
            Inline::Run(run) => push_slot_fonts(&run.properties.fonts, out),
            Inline::Hyperlink(link) => collect_inline_fonts(&link.inlines, out),
            Inline::Field(field) => collect_inline_fonts(&field.result, out),
            Inline::Revision(rev) => collect_inline_fonts(&rev.inlines, out),
            _ => {}
        }
    }
}

fn collect_block_fonts(block: &Block, out: &mut Vec<String>) {
    match block {
        Block::Paragraph(paragraph) => {
            push_slot_fonts(&paragraph.mark.fonts, out);
            collect_inline_fonts(&paragraph.inlines, out);
        }
        Block::Table(table) => {
            for row in &table.rows {
                for cell in &row.cells {
                    for inner in &cell.blocks {
                        collect_block_fonts(inner, out);
                    }
                }
            }
        }
    }
}

fn referenced_fonts(document: &Document) -> Vec<String> {
    let mut raw = Vec::new();
    for entry in &document.fonts {
        if !entry.name.trim().is_empty() {
            raw.push(entry.name.trim().to_owned());
        }
        if let Some(alt) = entry.alt_name.as_deref().map(str::trim)
            && !alt.is_empty()
        {
            raw.push(alt.to_owned());
        }
    }
    push_slot_fonts(&document.styles.default_run.fonts, &mut raw);
    for style in &document.styles.styles {
        push_slot_fonts(&style.run.fonts, &mut raw);
    }
    for block in document.sections.iter().flat_map(|s| &s.blocks) {
        collect_block_fonts(block, &mut raw);
    }
    let mut seen = HashSet::new();
    raw.retain(|name| seen.insert(normalize_font_name(name)));
    raw
}

fn is_allowed_external_url(raw: &str) -> bool {
    let trimmed = raw.trim();
    let Ok(parsed) = url::Url::parse(trimmed) else {
        return false;
    };
    match parsed.scheme() {
        "http" | "https" => parsed.has_host(),
        "mailto" => !parsed.path().is_empty(),
        _ => false,
    }
}

fn collect_inline_text(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Run(run) => {
                for item in &run.content {
                    if let RunContent::Text(text) = item {
                        out.push_str(text);
                    }
                }
            }
            Inline::Hyperlink(link) => collect_inline_text(&link.inlines, out),
            Inline::Field(field) => collect_inline_text(&field.result, out),
            Inline::Revision(rev) => collect_inline_text(&rev.inlines, out),
            _ => {}
        }
    }
}

fn collect_inlines_links(inlines: &[Inline], out: &mut Vec<(String, String)>) {
    for inline in inlines {
        match inline {
            Inline::Hyperlink(link) => {
                if let Some(target) = link.target.as_deref().map(str::trim)
                    && is_allowed_external_url(target)
                {
                    let mut text = String::new();
                    collect_inline_text(&link.inlines, &mut text);
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        out.push((trimmed.to_owned(), target.to_owned()));
                    }
                }
                collect_inlines_links(&link.inlines, out);
            }
            Inline::Field(field) => collect_inlines_links(&field.result, out),
            Inline::Revision(rev) => collect_inlines_links(&rev.inlines, out),
            _ => {}
        }
    }
}

fn collect_block_links(block: &Block, out: &mut Vec<(String, String)>) {
    match block {
        Block::Paragraph(paragraph) => collect_inlines_links(&paragraph.inlines, out),
        Block::Table(table) => {
            for row in &table.rows {
                for cell in &row.cells {
                    for inner in &cell.blocks {
                        collect_block_links(inner, out);
                    }
                }
            }
        }
    }
}

fn page_images(page: &Page) -> Vec<[f32; 4]> {
    let mut stack: Vec<[f32; 6]> = vec![[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]];
    let mut boxes = Vec::new();
    for item in &page.items {
        match item {
            Item::TransformBegin(m) => {
                let top = *stack.last().unwrap_or(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                // [a, b, c, d, e, f] composes top * m
                stack.push([
                    top[0] * m[0] + top[2] * m[1],
                    top[1] * m[0] + top[3] * m[1],
                    top[0] * m[2] + top[2] * m[3],
                    top[1] * m[2] + top[3] * m[3],
                    top[0] * m[4] + top[2] * m[5] + top[4],
                    top[1] * m[4] + top[3] * m[5] + top[5],
                ]);
            }
            Item::TransformEnd => {
                if stack.len() > 1 {
                    stack.pop();
                }
            }
            Item::Image { rect, .. } | Item::Picture { rect, .. } => {
                let m = *stack.last().unwrap_or(&[1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
                let corners = [
                    (rect.x, rect.y),
                    (rect.x + rect.width, rect.y),
                    (rect.x, rect.y + rect.height),
                    (rect.x + rect.width, rect.y + rect.height),
                ];
                let mut min_x = f32::INFINITY;
                let mut min_y = f32::INFINITY;
                let mut max_x = f32::NEG_INFINITY;
                let mut max_y = f32::NEG_INFINITY;
                for (x, y) in corners {
                    let tx = m[0] * x + m[2] * y + m[4];
                    let ty = m[1] * x + m[3] * y + m[5];
                    min_x = min_x.min(tx);
                    min_y = min_y.min(ty);
                    max_x = max_x.max(tx);
                    max_y = max_y.max(ty);
                }
                let x = min_x.clamp(0.0, page.width);
                let y = min_y.clamp(0.0, page.height);
                let right = max_x.clamp(0.0, page.width);
                let bottom = max_y.clamp(0.0, page.height);
                let w = right - x;
                let h = bottom - y;
                if w > 0.0 && h > 0.0 {
                    boxes.push([x, y, w, h]);
                }
            }
            _ => {}
        }
    }
    boxes
}

impl Word {
    pub fn open(path: &Path) -> Result<Self, String> {
        let data = crate::formats::read_bytes(path)?;
        let document = docboss_core::read(&data).map_err(|error| match error {
            Error::Encrypted | Error::Crypt(_) => "此 Word 文件受密码保护".to_owned(),
            other => format!("无法解析 Word 文件：{other}"),
        })?;
        let cjk = cjk_font_support();
        let referenced = referenced_fonts(&document);
        let referenced_norm: HashSet<String> = referenced
            .iter()
            .map(|name| normalize_font_name(name))
            .collect();
        let embedded_norm: HashSet<String> = document
            .fonts
            .iter()
            .filter(|font| {
                font.embedded_regular.is_some()
                    || font.embedded_bold.is_some()
                    || font.embedded_italic.is_some()
                    || font.embedded_bold_italic.is_some()
            })
            .flat_map(|font| {
                std::iter::once(normalize_font_name(&font.name))
                    .chain(font.alt_name.as_deref().map(normalize_font_name))
            })
            .collect();

        let mut fonts = FontDatabase::system();
        let default_norm = normalize_font_name(&cjk.default_family);
        let active_aliases: Vec<FontEntry> = cjk
            .alias_entries
            .iter()
            .filter(|entry| {
                let zh = normalize_font_name(&entry.name);
                let en = entry
                    .alt_name
                    .as_deref()
                    .map(normalize_font_name)
                    .unwrap_or_default();
                zh == default_norm
                    || en == default_norm
                    || referenced_norm.contains(&zh)
                    || referenced_norm.contains(&en)
            })
            .cloned()
            .collect();
        fonts.add_document_fonts(&active_aliases);
        fonts.add_document_fonts(&document.fonts);
        if let Some(default_reg) = cjk.default_regular.clone() {
            let missing_entries: Vec<FontEntry> = referenced
                .into_iter()
                .filter(|name| {
                    let key = normalize_font_name(name);
                    !key.is_empty()
                        && !embedded_norm.contains(&key)
                        && !cjk.known_families.contains(&key)
                })
                .map(|name| FontEntry {
                    name,
                    alt_name: None,
                    family: None,
                    pitch: None,
                    embedded_regular: Some(default_reg.clone()),
                    embedded_bold: cjk.default_bold.clone(),
                    embedded_italic: None,
                    embedded_bold_italic: None,
                })
                .collect();
            fonts.add_document_fonts(&missing_entries);
        }

        let fonts = Arc::new(fonts);
        let options = LayoutOptions {
            kerning: false,
            default_font: cjk.default_family.clone(),
        };
        let layout = docboss_layout::layout_with_options(&document, &fonts, &options);
        let pages = layout
            .pages
            .iter()
            .map(|page| [page.width, page.height])
            .collect::<Vec<_>>();
        if pages.is_empty() {
            return Err("Word 文件没有可显示的页面".into());
        }
        let images = layout.pages.iter().map(page_images).collect::<Vec<_>>();
        let mut block_links = Vec::new();
        let mut outline = Vec::new();
        for (number, block) in document
            .sections
            .iter()
            .flat_map(|section| &section.blocks)
            .enumerate()
        {
            let mut links = Vec::new();
            collect_block_links(block, &mut links);
            block_links.push(links);
            let Block::Paragraph(paragraph) = block else {
                continue;
            };
            let Some(level) = document
                .styles
                .heading_level(paragraph, &document.numbering)
            else {
                continue;
            };
            let title = paragraph.text();
            if title.trim().is_empty() {
                continue;
            }
            let index = layout
                .pages
                .iter()
                .position(|page| page.blocks.contains(&number))
                .unwrap_or(0);
            outline.push(Heading {
                title: format!(
                    "{}{}",
                    "　".repeat(level.saturating_sub(1).min(4) as usize),
                    title.trim()
                ),
                index,
            });
        }
        Ok(Self {
            metadata: Metadata {
                pages,
                images,
                outline,
            },
            layout,
            block_links,
        })
    }

    pub fn image(&self, index: usize, requested_scale: Option<f32>) -> Result<Vec<u8>, String> {
        let [width, height] = *self.metadata.pages.get(index).ok_or("页码不存在")?;
        let max_scale = (16_000_000.0 / (width * height).max(1.0))
            .sqrt()
            .clamp(0.5, 6.0);
        let scale = requested_scale
            .filter(|value| value.is_finite())
            .unwrap_or(2.0)
            .clamp(0.5, max_scale);
        let pixmap = docboss_render::render_page(&self.layout, index, scale)
            .map_err(|error| error.to_string())?;
        pixmap
            .encode(Format::Png)
            .map_err(|error| error.to_string())
    }

    pub fn text(&self, index: usize) -> Result<Vec<TextRun<'_>>, String> {
        let page = self.layout.pages.get(index).ok_or("页码不存在")?;
        let page_links: Vec<&(String, String)> = self
            .block_links
            .get(page.blocks.clone())
            .into_iter()
            .flatten()
            .flatten()
            .collect();
        let mut result = Vec::new();
        let mut baseline: Option<f32> = None;
        for run in page.glyph_runs() {
            if run.text.is_empty() || run.glyphs.is_empty() {
                continue;
            }
            let (left, width) = self.bounds(run);
            let line_break =
                baseline.is_some_and(|previous| (previous - run.baseline).abs() > run.size * 0.6);
            baseline = Some(run.baseline);
            let trimmed = run.text.trim();
            let trimmed_chars = trimmed.chars().count();
            let url = if trimmed_chars >= 2 {
                page_links
                    .iter()
                    .find(|(label, _)| {
                        label.contains(trimmed)
                            || (label.chars().count() >= 2 && trimmed.contains(label.as_str()))
                    })
                    .map(|(_, target)| target.clone())
            } else {
                None
            };
            result.push(TextRun {
                text: &run.text,
                left,
                width,
                top: run.baseline - run.size * 0.85,
                size: run.size,
                line_break,
                url,
            });
        }
        Ok(result)
    }

    fn bounds(&self, run: &GlyphRun) -> (f32, f32) {
        let face = self.layout.fonts.font(run.font);
        let mut left = f32::INFINITY;
        let mut right = f32::NEG_INFINITY;
        for glyph in &run.glyphs {
            let advance = face.as_ref().map_or(run.size * 0.55, |font| {
                f32::from(font.advance(glyph.id)) * run.size / f32::from(font.units_per_em().max(1))
            });
            left = left.min(glyph.x);
            right = right.max(glyph.x + advance);
        }
        (left, (right - left).max(0.1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use docboss_core::{Hyperlink, Paragraph, Run, RunProperties};

    #[test]
    fn normalizes_ascii_and_cjk_font_names() {
        assert_eq!(normalize_font_name("DengXian Light"), "dengxianlight");
        assert_eq!(normalize_font_name("等线"), "等线");
        assert_eq!(normalize_font_name("Microsoft YaHei"), "microsoftyahei");
        assert_eq!(normalize_font_name("楷体_GB2312"), "楷体gb2312");
    }

    #[test]
    fn collects_valid_external_links_from_word_blocks() {
        let run = |text: &str| {
            Inline::Run(Run {
                properties: RunProperties::default(),
                content: vec![RunContent::Text(text.to_owned())],
            })
        };
        let block = Block::Paragraph(Paragraph {
            inlines: vec![
                Inline::Hyperlink(Hyperlink {
                    target: Some("https://example.com/docs".to_owned()),
                    anchor: None,
                    tooltip: None,
                    inlines: vec![run("示例链接")],
                }),
                Inline::Hyperlink(Hyperlink {
                    target: Some("javascript:alert(1)".to_owned()),
                    anchor: None,
                    tooltip: None,
                    inlines: vec![run("危险脚本")],
                }),
                Inline::Hyperlink(Hyperlink {
                    target: None,
                    anchor: Some("sec1".to_owned()),
                    tooltip: None,
                    inlines: vec![run("内部书签")],
                }),
            ],
            ..Paragraph::default()
        });
        let mut links = Vec::new();
        collect_block_links(&block, &mut links);
        assert_eq!(
            links,
            vec![("示例链接".to_owned(), "https://example.com/docs".to_owned())]
        );
    }
}
