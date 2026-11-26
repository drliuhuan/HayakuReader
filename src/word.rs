use docboss_core::{Block, Error};
use docboss_font::FontDatabase;
use docboss_layout::{GlyphRun, Layout};
use docboss_render::Format;
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

#[derive(Clone, Serialize)]
pub struct Metadata {
    pub pages: Vec<[f32; 2]>,
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
}

pub struct Word {
    pub metadata: Metadata,
    layout: Layout,
}

fn system_fonts() -> FontDatabase {
    let mut database = FontDatabase::new();
    #[cfg(windows)]
    {
        if let Some(root) = std::env::var_os("WINDIR") {
            database.add_dir(&PathBuf::from(root).join("Fonts"));
        }
        if let Some(root) = std::env::var_os("LOCALAPPDATA") {
            database.add_dir(&PathBuf::from(root).join("Microsoft/Windows/Fonts"));
        }
    }
    #[cfg(not(windows))]
    for root in ["/usr/share/fonts", "/usr/local/share/fonts"] {
        database.add_dir(&PathBuf::from(root));
    }
    database
}

impl Word {
    pub fn open(path: &Path) -> Result<Self, String> {
        let data = crate::formats::read_bytes(path)?;
        let document = docboss_core::read(&data).map_err(|error| match error {
            Error::Encrypted | Error::Crypt(_) => "此 Word 文件受密码保护".to_owned(),
            other => format!("无法解析 Word 文件：{other}"),
        })?;
        static FONTS: OnceLock<Arc<FontDatabase>> = OnceLock::new();
        let has_embedded_fonts = document.fonts.iter().any(|font| {
            font.embedded_regular.is_some()
                || font.embedded_bold.is_some()
                || font.embedded_italic.is_some()
                || font.embedded_bold_italic.is_some()
        });
        let fonts = if has_embedded_fonts {
            let mut fonts = system_fonts();
            fonts.add_document_fonts(&document.fonts);
            Arc::new(fonts)
        } else {
            FONTS.get_or_init(|| Arc::new(system_fonts())).clone()
        };
        let layout = docboss_layout::layout(&document, &fonts);
        let pages = layout
            .pages
            .iter()
            .map(|page| [page.width, page.height])
            .collect::<Vec<_>>();
        if pages.is_empty() {
            return Err("Word 文件没有可显示的页面".into());
        }
        let mut outline = Vec::new();
        for (number, block) in document
            .sections
            .iter()
            .flat_map(|section| &section.blocks)
            .enumerate()
        {
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
            metadata: Metadata { pages, outline },
            layout,
        })
    }

    pub fn image(&self, index: usize) -> Result<Vec<u8>, String> {
        let [width, height] = *self.metadata.pages.get(index).ok_or("页码不存在")?;
        let scale = (12_000_000.0 / (width * height)).sqrt().clamp(0.1, 2.0);
        let pixmap = docboss_render::render_page(&self.layout, index, scale)
            .map_err(|error| error.to_string())?;
        pixmap
            .encode(Format::Png)
            .map_err(|error| error.to_string())
    }

    pub fn text(&self, index: usize) -> Result<Vec<TextRun<'_>>, String> {
        let page = self.layout.pages.get(index).ok_or("页码不存在")?;
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
            result.push(TextRun {
                text: &run.text,
                left,
                width,
                top: run.baseline - run.size * 0.85,
                size: run.size,
                line_break,
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
