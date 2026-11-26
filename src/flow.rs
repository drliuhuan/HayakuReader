use crate::math::mathml_element_to_latex;
use ego_tree::NodeRef;
use encoding_rs::{GB18030, UTF_8, UTF_16BE, UTF_16LE};
use pulldown_cmark::{CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd};
use scraper::{ElementRef, Html, Node};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::Path,
};
use zip::ZipArchive;

const MAX_RESOURCE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutlineEntry {
    pub title: String,
    pub index: usize,
    pub fragment: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkTarget {
    External(String),
    Internal { chapter: usize, fragment: String },
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)]
pub enum Inline {
    Text(String),
    Styled {
        bold: bool,
        italic: bool,
        strike: bool,
        code: bool,
        superscript: bool,
        subscript: bool,
        inlines: Vec<Inline>,
    },
    Link {
        target: LinkTarget,
        inlines: Vec<Inline>,
    },
    Ruby {
        base: String,
        annotation: String,
    },
    InlineMath {
        latex: String,
    },
    LineBreak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    Heading {
        level: u8,
        inlines: Vec<Inline>,
        anchor: Option<String>,
    },
    Paragraph(Vec<Inline>),
    Quote(Vec<Block>),
    List {
        ordered: bool,
        items: Vec<Vec<Block>>,
    },
    Code {
        lang: String,
        code: String,
    },
    Table {
        headers: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Image {
        path: String,
        alt: String,
    },
    DisplayMath {
        latex: String,
    },
    Rule,
}

#[derive(Clone, Debug)]
pub struct FlowChapter {
    #[allow(dead_code)]
    pub path: String,
    pub title: String,
    pub blocks: Vec<Block>,
    pub anchors: HashMap<String, usize>,
    pub raw_source: String,
}

#[derive(Clone, Debug)]
pub struct FlowDocument {
    pub kind: &'static str,
    pub chapters: Vec<FlowChapter>,
    pub outline: Vec<OutlineEntry>,
    pub resources: HashMap<String, Vec<u8>>,
}

impl FlowChapter {
    pub fn plain_text(&self) -> String {
        let mut out = String::new();
        for block in &self.blocks {
            collect_block_plain(block, &mut out);
        }
        out
    }
}

fn collect_inline_plain(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => out.push_str(text),
            Inline::Styled { inlines, .. } | Inline::Link { inlines, .. } => {
                collect_inline_plain(inlines, out);
            }
            Inline::Ruby { base, .. } => out.push_str(base),
            Inline::InlineMath { latex } => out.push_str(latex),
            Inline::LineBreak => out.push('\n'),
        }
    }
}

fn collect_block_plain(block: &Block, out: &mut String) {
    match block {
        Block::Heading { inlines, .. } | Block::Paragraph(inlines) => {
            collect_inline_plain(inlines, out);
            out.push('\n');
        }
        Block::Quote(blocks) => {
            for b in blocks {
                collect_block_plain(b, out);
            }
        }
        Block::List { items, .. } => {
            for item in items {
                for b in item {
                    collect_block_plain(b, out);
                }
            }
        }
        Block::Code { code, .. } => {
            out.push_str(code);
            out.push('\n');
        }
        Block::Table { headers, rows } => {
            for cell in headers {
                collect_inline_plain(cell, out);
                out.push('\t');
            }
            out.push('\n');
            for row in rows {
                for cell in row {
                    collect_inline_plain(cell, out);
                    out.push('\t');
                }
                out.push('\n');
            }
        }
        Block::Image { alt, .. } => {
            if !alt.is_empty() {
                out.push_str(alt);
                out.push('\n');
            }
        }
        Block::DisplayMath { latex } => {
            out.push_str(latex);
            out.push('\n');
        }
        Block::Rule => {}
    }
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    bounded_read(file)
}

fn bounded_read(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    reader
        .take(MAX_RESOURCE + 1)
        .read_to_end(&mut result)
        .map_err(|error| error.to_string())?;
    if result.len() as u64 > MAX_RESOURCE {
        return Err("文件或内嵌资源超过 64 MiB".into());
    }
    Ok(result)
}

pub fn decode(bytes: &[u8], label: Option<&str>) -> Result<String, String> {
    let encoding = match label {
        Some("utf-8") => UTF_8,
        Some("utf-16le") => UTF_16LE,
        Some("utf-16be") => UTF_16BE,
        Some("gb18030") => GB18030,
        Some(_) => return Err("不支持此编码".into()),
        None => encoding_rs::Encoding::for_bom(bytes)
            .map(|(encoding, _)| encoding)
            .unwrap_or(if std::str::from_utf8(bytes).is_ok() {
                UTF_8
            } else {
                GB18030
            }),
    };
    let (text, _, _) = encoding.decode(bytes);
    Ok(text.into_owned())
}

pub fn open_text(path: &Path, encoding: Option<&str>) -> Result<FlowDocument, String> {
    let raw = decode(&read_bytes(path)?, encoding)?;
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "文本".to_owned());
    let blocks = parse_plain_text_blocks(&raw);
    Ok(FlowDocument {
        kind: "text",
        chapters: vec![FlowChapter {
            path: String::new(),
            title,
            blocks,
            anchors: HashMap::new(),
            raw_source: raw,
        }],
        outline: Vec::new(),
        resources: HashMap::new(),
    })
}

pub fn open_markdown(path: &Path) -> Result<FlowDocument, String> {
    let raw = decode(&read_bytes(path)?, None)?;
    let base_dir = path.parent().map(Path::to_path_buf);
    Ok(parse_markdown_document(&raw, base_dir.as_deref()))
}

fn load_relative_resource(base: &Path, rel: &str, resources: &mut HashMap<String, Vec<u8>>) {
    if rel.is_empty() || rel.contains("://") || rel.starts_with("data:") || resources.contains_key(rel)
    {
        return;
    }
    let p = Path::new(rel);
    if !p
        .components()
        .all(|c| matches!(c, std::path::Component::Normal(_)))
    {
        return;
    }
    if let Ok(root) = base.canonicalize()
        && let Ok(cand) = root.join(p).canonicalize()
        && cand.starts_with(&root)
        && let Ok(bytes) = read_bytes(&cand)
    {
        resources.insert(rel.to_owned(), bytes);
    }
}

pub fn open_html(path: &Path) -> Result<FlowDocument, String> {
    let bytes = read_bytes(path)?;
    let html = decode(&bytes, None)?;
    let file_title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "HTML 文档".to_owned());
    let parsed_doc = Html::parse_document(&html);
    let head_title = scraper::Selector::parse("title")
        .ok()
        .and_then(|sel| parsed_doc.select(&sel).next())
        .map(|el| el.text().collect::<String>().trim().to_owned())
        .filter(|t| !t.is_empty())
        .unwrap_or(file_title);

    let (blocks, img_paths, anchors) =
        parse_xhtml_blocks_with_anchors(&html, "", 0, &HashMap::new());
    let mut outline = Vec::new();
    for block in &blocks {
        if let Block::Heading {
            level,
            inlines,
            anchor,
        } = block
        {
            let mut text = String::new();
            collect_inline_plain(inlines, &mut text);
            let trimmed = text.trim();
            if !trimmed.is_empty() {
                let indent = "　".repeat(level.saturating_sub(1).min(4) as usize);
                outline.push(OutlineEntry {
                    title: format!("{indent}{trimmed}"),
                    index: 0,
                    fragment: anchor.clone().unwrap_or_default(),
                });
            }
        }
    }
    let mut resources = HashMap::new();
    if let Some(base) = path.parent() {
        for rel in img_paths {
            load_relative_resource(base, &rel, &mut resources);
        }
    }
    Ok(FlowDocument {
        kind: "html",
        chapters: vec![FlowChapter {
            path: String::new(),
            title: head_title,
            blocks,
            anchors,
            raw_source: html,
        }],
        outline,
        resources,
    })
}

pub fn open_xml(path: &Path) -> Result<FlowDocument, String> {
    let bytes = read_bytes(path)?;
    let xml = decode(&bytes, None)?;
    let title = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "XML 文档".to_owned());
    let mut blocks = Vec::new();
    if let Ok(doc) = roxmltree::Document::parse(&xml) {
        let mut lines = Vec::new();
        for node in doc.descendants() {
            if node.is_text()
                && let Some(t) = node.text()
            {
                let trimmed = collapse_spaces(t.trim());
                if !trimmed.is_empty() {
                    lines.push(trimmed);
                }
            }
        }
        if lines.is_empty() {
            blocks.push(Block::Code {
                lang: "xml".to_owned(),
                code: xml.clone(),
            });
        } else {
            for line in lines {
                blocks.push(Block::Paragraph(vec![Inline::Text(line)]));
            }
        }
    } else {
        blocks.push(Block::Code {
            lang: "xml".to_owned(),
            code: xml.clone(),
        });
    }
    Ok(FlowDocument {
        kind: "xml",
        chapters: vec![FlowChapter {
            path: String::new(),
            title,
            blocks,
            anchors: HashMap::new(),
            raw_source: xml,
        }],
        outline: Vec::new(),
        resources: HashMap::new(),
    })
}

pub fn open_epub(path: &Path) -> Result<FlowDocument, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|error| error.to_string())?;
    let (raw_chapters, outline) = epub_chapters(&mut zip)?;
    let index_map = build_chapter_index_map(&raw_chapters);
    let mut resources = HashMap::new();
    let mut chapters = Vec::with_capacity(raw_chapters.len());
    for (idx, ch) in raw_chapters.into_iter().enumerate() {
        let data = zip_entry(&mut zip, &ch.path)?;
        let xhtml = decode(&data, None)?;
        let (blocks, img_paths, anchors) =
            parse_xhtml_blocks_with_anchors(&xhtml, &ch.path, idx, &index_map);
        for res_path in img_paths {
            if !resources.contains_key(&res_path)
                && let Ok(bytes) = zip_entry(&mut zip, &res_path)
            {
                resources.insert(res_path, bytes);
            }
        }
        chapters.push(FlowChapter {
            path: ch.path,
            title: ch.title,
            blocks,
            anchors,
            raw_source: xhtml,
        });
    }
    let mut final_outline = outline;
    if final_outline.is_empty() {
        for (idx, chapter) in chapters.iter().enumerate() {
            final_outline.push(OutlineEntry {
                title: chapter.title.clone(),
                index: idx,
                fragment: String::new(),
            });
        }
    }
    Ok(FlowDocument {
        kind: "epub",
        chapters,
        outline: final_outline,
        resources,
    })
}

pub fn open_kindle(path: &Path) -> Result<FlowDocument, String> {
    let data = read_bytes(path)?;
    let book = ebook_rs::MobiBook::parse(&data).map_err(|error| match error {
        ebook_rs::EbookError::DrmProtected(_) => "此电子书受 DRM 保护".to_owned(),
        other if other.to_string().contains("HUFF/CDIC") => {
            "暂不支持使用 HUFF/CDIC 压缩的 Kindle 文件".to_owned()
        }
        other => format!("无法解析 Kindle 文件：{other}"),
    })?;
    let mut raw_chapters = book
        .sections()
        .iter()
        .enumerate()
        .map(|(index, section)| ChapterMeta {
            path: section.full_path.clone(),
            title: format!("第 {} 章", index + 1),
        })
        .collect::<Vec<_>>();
    if raw_chapters.is_empty() {
        return Err("Kindle 文件没有可读章节".into());
    }
    let section_index = build_chapter_index_map(&raw_chapters);
    let mut outline = Vec::new();
    collect_kindle_toc(
        book.toc(),
        0,
        &section_index,
        &mut outline,
        &mut raw_chapters,
    );

    let mut resources = HashMap::new();
    let mut chapters = Vec::with_capacity(raw_chapters.len());
    for (idx, meta) in raw_chapters.into_iter().enumerate() {
        let section = book
            .get_section_raw(idx)
            .ok_or_else(|| "章节不存在".to_owned())?;
        let xhtml = section.raw_html.clone();
        let (blocks, img_paths, anchors) =
            parse_xhtml_blocks_with_anchors(&xhtml, &meta.path, idx, &section_index);
        for res_path in img_paths {
            if !resources.contains_key(&res_path)
                && let Ok((bytes, _)) = book
                    .get_resource_bytes(&res_path)
                    .or_else(|_| book.get_resource_bytes(&format!("OEBPS/{res_path}")))
            {
                resources.insert(res_path, bytes);
            }
        }
        chapters.push(FlowChapter {
            path: meta.path,
            title: meta.title,
            blocks,
            anchors,
            raw_source: xhtml,
        });
    }
    if outline.is_empty() {
        for (idx, chapter) in chapters.iter().enumerate() {
            outline.push(OutlineEntry {
                title: chapter.title.clone(),
                index: idx,
                fragment: String::new(),
            });
        }
    }
    Ok(FlowDocument {
        kind: "kindle",
        chapters,
        outline,
        resources,
    })
}

fn find_closing_delim(s: &str, open_end: usize, close: &str, allow_newline: bool) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut i = open_end;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if !allow_newline && bytes[i] == b'\n' {
            return None;
        }
        if s[i..].starts_with(close) {
            return Some(i);
        }
        let ch = s[i..].chars().next()?;
        i += ch.len_utf8();
    }
    None
}

fn find_next_math(s: &str, from: usize) -> Option<(usize, usize, String, bool)> {
    let mut i = from;
    let bytes = s.as_bytes();
    while i < bytes.len() {
        if bytes[i] == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if next == b'[' {
                if let Some(end) = find_closing_delim(s, i + 2, r"\]", true) {
                    let latex = s[i + 2..end].trim().to_owned();
                    if !latex.is_empty() {
                        return Some((i, end + 2, latex, false));
                    }
                }
            } else if next == b'(' {
                if let Some(end) = find_closing_delim(s, i + 2, r"\)", false) {
                    let latex = s[i + 2..end].trim().to_owned();
                    if !latex.is_empty() {
                        return Some((i, end + 2, latex, false));
                    }
                }
            } else {
                i += 2;
                continue;
            }
        } else if s[i..].starts_with("$$") {
            if let Some(end) = find_closing_delim(s, i + 2, "$$", true) {
                let latex = s[i + 2..end].trim().to_owned();
                if !latex.is_empty() {
                    return Some((i, end + 2, latex, false));
                }
            }
        } else if bytes[i] == b'$' {
            // Avoid mistaking escaped \$ or currency like "$100 ... $200" where next $ is preceded by space
            let escaped = i > 0 && bytes[i - 1] == b'\\';
            if !escaped {
                let after = s[i + 1..].chars().next();
                if after.is_some_and(|c| c != ' ' && c != '\t' && c != '\n')
                    && let Some(end) = find_closing_delim(s, i + 1, "$", false)
                {
                    let prev = s[..end].chars().next_back();
                    if prev.is_some_and(|c| c != ' ' && c != '\t') {
                        let latex = s[i + 1..end].trim().to_owned();
                        if !latex.is_empty() {
                            return Some((i, end + 1, latex, false));
                        }
                    }
                }
            }
        }
        let ch = s[i..].chars().next()?;
        i += ch.len_utf8();
    }
    None
}

/// Checks whether a formula starting at byte index `idx` inside `text` is preceded by a newline.
/// Rules from user:
/// - No newline immediately before the formula (ignoring spaces/tabs): render INLINE (`Inline::InlineMath`).
/// - Newline immediately before the formula (ignoring spaces/tabs between `\n` and delimiter):
///   render on its own line (`Block::DisplayMath`).
fn preceded_by_newline(text: &str, idx: usize, at_line_after_newline: bool) -> bool {
    let prefix = &text[..idx];
    if let Some(nl_pos) = prefix.rfind('\n') {
        prefix[nl_pos + 1..].trim_matches([' ', '\t', '\r']).is_empty()
    } else {
        at_line_after_newline && prefix.trim_matches([' ', '\t', '\r']).is_empty()
    }
}

fn push_plain_segment_with_links(text: &str, out: &mut Vec<Inline>) {
    let mut last = 0usize;
    let mut scan = 0usize;
    while scan < text.len() {
        let rest = &text[scan..];
        let rel = rest.find("http://").or_else(|| rest.find("https://"));
        let Some(offset) = rel else {
            break;
        };
        let start = scan + offset;
        let after = &text[start..];
        let mut end_rel = after
            .find(|c: char| {
                c.is_whitespace()
                    || matches!(
                        c,
                        '<' | '>'
                            | '"'
                            | '\''
                            | '`'
                            | '('
                            | ')'
                            | '['
                            | ']'
                            | '{'
                            | '}'
                            | '，'
                            | '。'
                            | '、'
                            | '；'
                            | '：'
                            | '！'
                            | '？'
                            | '（'
                            | '）'
                            | '【'
                            | '】'
                            | '《'
                            | '》'
                    )
            })
            .unwrap_or(after.len());
        let mut url_slice = &after[..end_rel];
        url_slice = url_slice.trim_end_matches(['.', ',', ';', ':', '!', '?']);
        end_rel = url_slice.len();
        if end_rel > "https://".len()
            && url::Url::parse(url_slice).is_ok_and(|u| u.has_host())
        {
            if start > last {
                out.push(Inline::Text(text[last..start].to_owned()));
            }
            out.push(Inline::Link {
                target: LinkTarget::External(url_slice.to_owned()),
                inlines: vec![Inline::Text(url_slice.to_owned())],
            });
            last = start + end_rel;
            scan = last;
        } else {
            scan = start + 7;
        }
    }
    if last < text.len() {
        out.push(Inline::Text(text[last..].to_owned()));
    }
}

pub enum MathSegment<'a> {
    Text(&'a str),
    Inline(String),
    Display(String),
}

pub fn split_math_segments(text: &str, start_after_newline: bool) -> Vec<MathSegment<'_>> {
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut line_after_nl = start_after_newline;
    while let Some((start, end, latex, _)) = find_next_math(text, pos) {
        let is_display = preceded_by_newline(text, start, line_after_nl);
        let before = &text[pos..start];
        if !before.is_empty() {
            let trimmed_before = if is_display {
                before.trim_end_matches([' ', '\t', '\r', '\n'])
            } else {
                before
            };
            if !trimmed_before.is_empty() {
                out.push(MathSegment::Text(trimmed_before));
            }
        }
        if is_display {
            out.push(MathSegment::Display(latex));
            // Consume trailing spaces on the display-formula line and single optional newline
            let mut next_pos = end;
            while next_pos < text.len() && matches!(text.as_bytes()[next_pos], b' ' | b'\t' | b'\r')
            {
                next_pos += 1;
            }
            if next_pos < text.len() && text.as_bytes()[next_pos] == b'\n' {
                next_pos += 1;
            }
            pos = next_pos;
            line_after_nl = true;
        } else {
            out.push(MathSegment::Inline(latex));
            pos = end;
            line_after_nl = false;
        }
    }
    if pos < text.len() {
        out.push(MathSegment::Text(&text[pos..]));
    }
    out
}

pub fn parse_plain_text_blocks(raw: &str) -> Vec<Block> {
    let normalized = raw.replace("\r\n", "\n");
    let mut blocks = Vec::new();
    let mut offset = 0usize;
    let bytes = normalized.as_bytes();
    let mut para_idx = 0usize;
    while offset < normalized.len() {
        // Find paragraph break (\n\n)
        let mut end = normalized.len();
        let mut i = offset;
        while i + 1 < bytes.len() {
            if bytes[i] == b'\n' && bytes[i + 1] == b'\n' {
                end = i;
                break;
            }
            i += 1;
        }
        let chunk = normalized[offset..end].trim_matches('\n');
        if !chunk.trim().is_empty() {
            let after_nl = offset > 0 && para_idx > 0;
            let segments = split_math_segments(chunk, after_nl);
            let mut current_inlines = Vec::new();
            for seg in segments {
                match seg {
                    MathSegment::Text(t) => push_plain_segment_with_links(t, &mut current_inlines),
                    MathSegment::Inline(latex) => current_inlines.push(Inline::InlineMath { latex }),
                    MathSegment::Display(latex) => {
                        if !current_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(&mut current_inlines)));
                        }
                        blocks.push(Block::DisplayMath { latex });
                    }
                }
            }
            if !current_inlines.is_empty() {
                blocks.push(Block::Paragraph(current_inlines));
            }
            para_idx += 1;
        }
        offset = if end < normalized.len() {
            let mut next = end;
            while next < bytes.len() && bytes[next] == b'\n' {
                next += 1;
            }
            next
        } else {
            end
        };
    }
    blocks
}

fn push_inline_into_current(
    inlines: Vec<Inline>,
    style_bold: bool,
    style_italic: bool,
    style_strike: bool,
    link_target: &Option<LinkTarget>,
    link_inlines: &mut Vec<Inline>,
    para_inlines: &mut Vec<Inline>,
) {
    let wrapped = if style_bold || style_italic || style_strike {
        vec![Inline::Styled {
            bold: style_bold,
            italic: style_italic,
            strike: style_strike,
            code: false,
            superscript: false,
            subscript: false,
            inlines,
        }]
    } else {
        inlines
    };
    if link_target.is_some() {
        link_inlines.extend(wrapped);
    } else {
        para_inlines.extend(wrapped);
    }
}

pub fn parse_markdown_document(raw: &str, base_dir: Option<&Path>) -> FlowDocument {
    let normalized = raw.replace("\r\n", "\n");
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let parser = Parser::new_ext(&normalized, options).into_offset_iter();

    let mut blocks: Vec<Block> = Vec::new();
    let mut resources = HashMap::new();
    let mut outline = Vec::new();

    #[derive(Clone, Copy)]
    enum ContainerKind {
        Paragraph,
        Heading,
        Quote,
        List,
        ListItem,
        Code,
        Table,
        TableHead,
        TableRow,
        TableCell,
    }

    let mut block_stack: Vec<ContainerKind> = Vec::new();
    let mut quote_blocks: Vec<Vec<Block>> = Vec::new();
    let mut list_stack: Vec<(bool, Vec<Vec<Block>>)> = Vec::new();
    let mut current_item_blocks: Vec<Vec<Block>> = Vec::new();

    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut link_target: Option<LinkTarget> = None;
    let mut link_inlines: Vec<Inline> = Vec::new();
    let mut cur_inlines: Vec<Inline> = Vec::new();
    let mut cur_heading_level = 1u8;
    let mut code_buf = String::new();
    let mut code_lang = String::new();
    let mut table_headers: Vec<Vec<Inline>> = Vec::new();
    let mut table_rows: Vec<Vec<Vec<Inline>>> = Vec::new();
    let mut cur_row: Vec<Vec<Inline>> = Vec::new();
    let mut in_image = false;
    let mut image_url = String::new();
    let mut image_alt = String::new();
    let mut pending_blocks: Vec<Block> = Vec::new();
    let mut had_inline_on_line = false;

    let flush_block = |blk: Block,
                       block_stack: &[ContainerKind],
                       quote_blocks: &mut [Vec<Block>],
                       current_item_blocks: &mut [Vec<Block>],
                       top_blocks: &mut Vec<Block>| {
        if let Some(last) = block_stack.iter().rev().find(|k| {
            matches!(k, ContainerKind::Quote | ContainerKind::ListItem)
        }) {
            match last {
                ContainerKind::Quote => {
                    if let Some(q) = quote_blocks.last_mut() {
                        q.push(blk);
                        return;
                    }
                }
                ContainerKind::ListItem => {
                    if let Some(item) = current_item_blocks.last_mut() {
                        item.push(blk);
                        return;
                    }
                }
                _ => {}
            }
        }
        top_blocks.push(blk);
    };

    for (event, range) in parser {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {
                    block_stack.push(ContainerKind::Paragraph);
                    cur_inlines.clear();
                    pending_blocks.clear();
                    had_inline_on_line = false;
                }
                Tag::Heading { level, .. } => {
                    cur_heading_level = match level {
                        HeadingLevel::H1 => 1,
                        HeadingLevel::H2 => 2,
                        HeadingLevel::H3 => 3,
                        HeadingLevel::H4 => 4,
                        HeadingLevel::H5 => 5,
                        HeadingLevel::H6 => 6,
                    };
                    block_stack.push(ContainerKind::Heading);
                    cur_inlines.clear();
                    had_inline_on_line = false;
                }
                Tag::BlockQuote(_) => {
                    block_stack.push(ContainerKind::Quote);
                    quote_blocks.push(Vec::new());
                }
                Tag::List(first) => {
                    block_stack.push(ContainerKind::List);
                    list_stack.push((first.is_some(), Vec::new()));
                }
                Tag::Item => {
                    block_stack.push(ContainerKind::ListItem);
                    current_item_blocks.push(Vec::new());
                }
                Tag::CodeBlock(kind) => {
                    block_stack.push(ContainerKind::Code);
                    code_buf.clear();
                    code_lang = match kind {
                        CodeBlockKind::Fenced(info) => info
                            .split_whitespace()
                            .next()
                            .unwrap_or("")
                            .to_owned(),
                        CodeBlockKind::Indented => String::new(),
                    };
                }
                Tag::Table(_) => {
                    block_stack.push(ContainerKind::Table);
                    table_headers.clear();
                    table_rows.clear();
                }
                Tag::TableHead => {
                    block_stack.push(ContainerKind::TableHead);
                    cur_row.clear();
                }
                Tag::TableRow => {
                    block_stack.push(ContainerKind::TableRow);
                    cur_row.clear();
                }
                Tag::TableCell => {
                    block_stack.push(ContainerKind::TableCell);
                    cur_inlines.clear();
                }
                Tag::Strong => bold = true,
                Tag::Emphasis => italic = true,
                Tag::Strikethrough => strike = true,
                Tag::Link { dest_url, .. } => {
                    let href = dest_url.trim().to_owned();
                    let target = if let Some(frag) = href.strip_prefix('#') {
                        LinkTarget::Internal {
                            chapter: 0,
                            fragment: frag.to_owned(),
                        }
                    } else {
                        LinkTarget::External(href)
                    };
                    link_target = Some(target);
                    link_inlines.clear();
                }
                Tag::Image { dest_url, .. } => {
                    in_image = true;
                    image_url = dest_url.trim().to_owned();
                    image_alt.clear();
                }
                _ => {}
            },
            Event::End(tag_end) => match tag_end {
                TagEnd::Paragraph => {
                    block_stack.pop();
                    for blk in pending_blocks.drain(..) {
                        flush_block(
                            blk,
                            &block_stack,
                            &mut quote_blocks,
                            &mut current_item_blocks,
                            &mut blocks,
                        );
                    }
                    if !cur_inlines.is_empty() {
                        flush_block(
                            Block::Paragraph(std::mem::take(&mut cur_inlines)),
                            &block_stack,
                            &mut quote_blocks,
                            &mut current_item_blocks,
                            &mut blocks,
                        );
                    }
                }
                TagEnd::Heading(_) => {
                    block_stack.pop();
                    let inlines = std::mem::take(&mut cur_inlines);
                    let mut heading_text = String::new();
                    collect_inline_plain(&inlines, &mut heading_text);
                    let trimmed = heading_text.trim();
                    if !trimmed.is_empty() {
                        let indent = "　".repeat(cur_heading_level.saturating_sub(1).min(4) as usize);
                        outline.push(OutlineEntry {
                            title: format!("{indent}{trimmed}"),
                            index: 0,
                            fragment: format!("h-{}", outline.len()),
                        });
                    }
                    flush_block(
                        Block::Heading {
                            level: cur_heading_level,
                            inlines,
                            anchor: outline.last().map(|o| o.fragment.clone()),
                        },
                        &block_stack,
                        &mut quote_blocks,
                        &mut current_item_blocks,
                        &mut blocks,
                    );
                }
                TagEnd::BlockQuote(_) => {
                    block_stack.pop();
                    if let Some(qb) = quote_blocks.pop() {
                        flush_block(
                            Block::Quote(qb),
                            &block_stack,
                            &mut quote_blocks,
                            &mut current_item_blocks,
                            &mut blocks,
                        );
                    }
                }
                TagEnd::Item => {
                    block_stack.pop();
                    if !cur_inlines.is_empty() {
                        let p = Block::Paragraph(std::mem::take(&mut cur_inlines));
                        if let Some(item) = current_item_blocks.last_mut() {
                            item.push(p);
                        }
                    }
                    if let Some(item) = current_item_blocks.pop()
                        && let Some((_, items)) = list_stack.last_mut()
                    {
                        items.push(item);
                    }
                }
                TagEnd::List(_) => {
                    block_stack.pop();
                    if let Some((ordered, items)) = list_stack.pop() {
                        flush_block(
                            Block::List { ordered, items },
                            &block_stack,
                            &mut quote_blocks,
                            &mut current_item_blocks,
                            &mut blocks,
                        );
                    }
                }
                TagEnd::CodeBlock => {
                    block_stack.pop();
                    flush_block(
                        Block::Code {
                            lang: std::mem::take(&mut code_lang),
                            code: std::mem::take(&mut code_buf),
                        },
                        &block_stack,
                        &mut quote_blocks,
                        &mut current_item_blocks,
                        &mut blocks,
                    );
                }
                TagEnd::TableHead => {
                    block_stack.pop();
                    table_headers = std::mem::take(&mut cur_row);
                }
                TagEnd::TableRow => {
                    block_stack.pop();
                    table_rows.push(std::mem::take(&mut cur_row));
                }
                TagEnd::TableCell => {
                    block_stack.pop();
                    cur_row.push(std::mem::take(&mut cur_inlines));
                }
                TagEnd::Table => {
                    block_stack.pop();
                    flush_block(
                        Block::Table {
                            headers: std::mem::take(&mut table_headers),
                            rows: std::mem::take(&mut table_rows),
                        },
                        &block_stack,
                        &mut quote_blocks,
                        &mut current_item_blocks,
                        &mut blocks,
                    );
                }
                TagEnd::Strong => bold = false,
                TagEnd::Emphasis => italic = false,
                TagEnd::Strikethrough => strike = false,
                TagEnd::Link => {
                    if let Some(target) = link_target.take() {
                        cur_inlines.push(Inline::Link {
                            target,
                            inlines: std::mem::take(&mut link_inlines),
                        });
                    }
                }
                TagEnd::Image => {
                    in_image = false;
                    let rel = image_url.clone();
                    if let Some(base) = base_dir {
                        load_relative_resource(base, &rel, &mut resources);
                    }
                    pending_blocks.push(Block::Image {
                        path: rel,
                        alt: std::mem::take(&mut image_alt),
                    });
                }
                _ => {}
            },
            Event::Text(text) => {
                if in_image {
                    image_alt.push_str(&text);
                    continue;
                }
                if matches!(block_stack.last(), Some(ContainerKind::Code)) {
                    code_buf.push_str(&text);
                    continue;
                }
                // Check whether the start of this event text is preceded by a newline in the raw source.
                let start_after_nl = range.start > 0
                    && normalized.as_bytes().get(range.start - 1) == Some(&b'\n')
                    && !had_inline_on_line;
                let in_paragraph = matches!(block_stack.last(), Some(ContainerKind::Paragraph));
                if in_paragraph && link_target.is_none() && !bold && !italic && !strike {
                    for seg in split_math_segments(&text, start_after_nl) {
                        match seg {
                            MathSegment::Text(t) => {
                                let mut chunk = Vec::new();
                                push_plain_segment_with_links(t, &mut chunk);
                                if !chunk.is_empty() {
                                    had_inline_on_line = true;
                                }
                                cur_inlines.extend(chunk);
                            }
                            MathSegment::Inline(latex) => {
                                had_inline_on_line = true;
                                cur_inlines.push(Inline::InlineMath { latex });
                            }
                            MathSegment::Display(latex) => {
                                if matches!(cur_inlines.last(), Some(Inline::Text(t)) if t == " ") {
                                    cur_inlines.pop();
                                }
                                if !cur_inlines.is_empty() {
                                    pending_blocks.push(Block::Paragraph(std::mem::take(&mut cur_inlines)));
                                }
                                pending_blocks.push(Block::DisplayMath { latex });
                                had_inline_on_line = false;
                            }
                        }
                    }
                } else {
                    let mut inlines = Vec::new();
                    for seg in split_math_segments(&text, false) {
                        match seg {
                            MathSegment::Text(t) => push_plain_segment_with_links(t, &mut inlines),
                            MathSegment::Inline(latex) | MathSegment::Display(latex) => {
                                inlines.push(Inline::InlineMath { latex });
                            }
                        }
                    }
                    if !inlines.is_empty() {
                        had_inline_on_line = true;
                    }
                    push_inline_into_current(
                        inlines,
                        bold,
                        italic,
                        strike,
                        &link_target,
                        &mut link_inlines,
                        &mut cur_inlines,
                    );
                }
            }
            Event::Code(code) => {
                had_inline_on_line = true;
                push_inline_into_current(
                    vec![Inline::Styled {
                        bold: false,
                        italic: false,
                        strike: false,
                        code: true,
                        superscript: false,
                        subscript: false,
                        inlines: vec![Inline::Text(code.into_string())],
                    }],
                    bold,
                    italic,
                    strike,
                    &link_target,
                    &mut link_inlines,
                    &mut cur_inlines,
                );
            }
            Event::SoftBreak => {
                if matches!(block_stack.last(), Some(ContainerKind::Code)) {
                    code_buf.push('\n');
                } else {
                    if !cur_inlines.is_empty() {
                        cur_inlines.push(Inline::Text(" ".to_owned()));
                    }
                    had_inline_on_line = false;
                }
            }
            Event::HardBreak => {
                if matches!(block_stack.last(), Some(ContainerKind::Code)) {
                    code_buf.push('\n');
                } else {
                    cur_inlines.push(Inline::LineBreak);
                    had_inline_on_line = false;
                }
            }
            Event::Rule => {
                flush_block(
                    Block::Rule,
                    &block_stack,
                    &mut quote_blocks,
                    &mut current_item_blocks,
                    &mut blocks,
                );
            }
            Event::TaskListMarker(checked) => {
                had_inline_on_line = true;
                cur_inlines.push(Inline::Text(if checked { "☑ " } else { "☐ " }.to_owned()));
            }
            _ => {}
        }
    }

    FlowDocument {
        kind: "markdown",
        chapters: vec![FlowChapter {
            path: String::new(),
            title: String::new(),
            blocks,
            anchors: outline
                .iter()
                .enumerate()
                .map(|(i, o)| (o.fragment.clone(), i))
                .collect(),
            raw_source: raw.to_owned(),
        }],
        outline,
        resources,
    }
}

pub fn parse_xhtml_blocks_with_anchors(
    xhtml: &str,
    base_path: &str,
    chapter_index: usize,
    chapter_index_map: &HashMap<String, usize>,
) -> (Vec<Block>, Vec<String>, HashMap<String, usize>) {
    let doc = Html::parse_document(xhtml);
    let root_ref = if let Some(body_el) = scraper::Selector::parse("body")
        .ok()
        .and_then(|sel| doc.select(&sel).next())
    {
        *body_el
    } else {
        *doc.root_element()
    };
    let mut ctx = XhtmlCtx {
        base_path,
        chapter_index,
        chapter_index_map,
        image_paths: Vec::new(),
        anchors: HashMap::new(),
    };
    let mut blocks = Vec::new();
    let mut loose_inlines = Vec::new();
    collect_xhtml_blocks(root_ref, &mut ctx, &mut blocks, &mut loose_inlines);
    if !loose_inlines.is_empty() {
        blocks.push(Block::Paragraph(loose_inlines));
    }
    (blocks, ctx.image_paths, ctx.anchors)
}

struct XhtmlCtx<'a> {
    base_path: &'a str,
    chapter_index: usize,
    chapter_index_map: &'a HashMap<String, usize>,
    image_paths: Vec<String>,
    anchors: HashMap<String, usize>,
}

fn record_node_anchor(node: NodeRef<'_, Node>, ctx: &mut XhtmlCtx<'_>, block_idx: usize) {
    if let Node::Element(el) = node.value() {
        if let Some(id) = el.attr("id").map(str::trim).filter(|s| !s.is_empty()) {
            ctx.anchors.entry(id.to_owned()).or_insert(block_idx);
        }
        if el.name() == "a"
            && let Some(name) = el.attr("name").map(str::trim).filter(|s| !s.is_empty())
        {
            ctx.anchors.entry(name.to_owned()).or_insert(block_idx);
        }
    }
}

fn collect_ruby_inlines(ruby_node: NodeRef<'_, Node>, out: &mut Vec<Inline>) {
    let mut pending_base = String::new();
    let mut has_rt = false;
    for child in ruby_node.children() {
        match child.value() {
            Node::Text(t) => {
                let s = t.text.trim();
                if !s.is_empty() {
                    pending_base.push_str(s);
                }
            }
            Node::Element(el) => match el.name() {
                "rp" => {}
                "rb" | "rbc" => {
                    if has_rt && !pending_base.is_empty() {
                        out.push(Inline::Text(std::mem::take(&mut pending_base)));
                        has_rt = false;
                    }
                    collect_plain_element_text(child, &mut pending_base);
                }
                "rt" | "rtc" => {
                    let mut rt_text = String::new();
                    collect_plain_element_text(child, &mut rt_text);
                    let rt_trimmed = rt_text.trim().to_owned();
                    let base_trimmed = std::mem::take(&mut pending_base).trim().to_owned();
                    if !base_trimmed.is_empty() {
                        if !rt_trimmed.is_empty() {
                            out.push(Inline::Ruby {
                                base: base_trimmed,
                                annotation: rt_trimmed,
                            });
                        } else {
                            out.push(Inline::Text(base_trimmed));
                        }
                    }
                    has_rt = true;
                }
                "ruby" => collect_ruby_inlines(child, out),
                _ => collect_plain_element_text(child, &mut pending_base),
            },
            _ => {}
        }
    }
    if !pending_base.trim().is_empty() {
        out.push(Inline::Text(pending_base.trim().to_owned()));
    }
}

fn collect_plain_element_text(node: NodeRef<'_, Node>, out: &mut String) {
    match node.value() {
        Node::Text(t) => {
            out.push_str(&t.text);
            return;
        }
        Node::Element(el) if el.name() == "rp" => return,
        _ => {}
    }
    for child in node.children() {
        collect_plain_element_text(child, out);
    }
}

fn parse_link_target(href: &str, ctx: &XhtmlCtx<'_>) -> Option<LinkTarget> {
    let trimmed = href.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(frag) = trimmed.strip_prefix('#') {
        return Some(LinkTarget::Internal {
            chapter: ctx.chapter_index,
            fragment: percent_decode(frag).unwrap_or_else(|| frag.to_owned()),
        });
    }
    if let Some((path, fragment)) = resolve_with_fragment(ctx.base_path, trimmed)
        && let Some(&chapter) = ctx.chapter_index_map.get(&path)
    {
        return Some(LinkTarget::Internal { chapter, fragment });
    }
    if trimmed.starts_with("http://")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("mailto:")
    {
        return Some(LinkTarget::External(trimmed.to_owned()));
    }
    None
}

fn has_meaningful_following_siblings(node: NodeRef<'_, Node>) -> bool {
    let mut cur = node.next_sibling();
    while let Some(sib) = cur {
        match sib.value() {
            Node::Text(t) if !t.text.trim().is_empty() => return true,
            Node::Element(el) if !matches!(el.name(), "script" | "style" | "br") => return true,
            _ => {}
        }
        cur = sib.next_sibling();
    }
    false
}

fn collect_xhtml_inlines(
    node: NodeRef<'_, Node>,
    ctx: &mut XhtmlCtx<'_>,
    blocks: &mut Vec<Block>,
    cur_inlines: &mut Vec<Inline>,
    ended_with_newline: &mut bool,
) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => {
                let text = t.text.as_ref();
                if text.is_empty() {
                    continue;
                }
                let collapse_ws = text.replace(['\r', '\t'], " ");
                if !collapse_ws.contains('$')
                    && !collapse_ws.contains(r"\(")
                    && !collapse_ws.contains(r"\[")
                {
                    let compacted = collapse_spaces(&collapse_ws);
                    if !compacted.is_empty() {
                        *ended_with_newline = collapse_ws.ends_with('\n');
                        push_plain_segment_with_links(&compacted, cur_inlines);
                    } else if collapse_ws.contains('\n') && !cur_inlines.is_empty() {
                        *ended_with_newline = true;
                    }
                    continue;
                }
                let start_nl = *ended_with_newline;
                for seg in split_math_segments(&collapse_ws, start_nl) {
                    match seg {
                        MathSegment::Text(t) => {
                            let compacted = collapse_spaces(t);
                            if !compacted.is_empty() {
                                *ended_with_newline = t.ends_with('\n');
                                push_plain_segment_with_links(&compacted, cur_inlines);
                            }
                        }
                        MathSegment::Inline(latex) => {
                            *ended_with_newline = false;
                            cur_inlines.push(Inline::InlineMath { latex });
                        }
                        MathSegment::Display(latex) => {
                            if !cur_inlines.is_empty() {
                                blocks.push(Block::Paragraph(std::mem::take(cur_inlines)));
                            }
                            blocks.push(Block::DisplayMath { latex });
                            *ended_with_newline = true;
                        }
                    }
                }
            }
            Node::Element(el) => {
                record_node_anchor(child, ctx, blocks.len());
                match el.name() {
                    "script" | "style" | "head" | "rp" => {}
                    "br" => {
                        cur_inlines.push(Inline::LineBreak);
                        *ended_with_newline = true;
                    }
                    "ruby" => {
                        *ended_with_newline = false;
                        collect_ruby_inlines(child, cur_inlines);
                    }
                    "math" => {
                        if let Some(el_ref) = ElementRef::wrap(child) {
                            let latex = mathml_element_to_latex(el_ref);
                            if !latex.trim().is_empty() {
                                let is_sole_in_block =
                                    cur_inlines.is_empty() && !has_meaningful_following_siblings(child);
                                let is_block = el.attr("display") == Some("block")
                                    || *ended_with_newline
                                    || is_sole_in_block;
                                if is_block {
                                    if !cur_inlines.is_empty() {
                                        blocks.push(Block::Paragraph(std::mem::take(cur_inlines)));
                                    }
                                    blocks.push(Block::DisplayMath { latex });
                                    *ended_with_newline = true;
                                } else {
                                    *ended_with_newline = false;
                                    cur_inlines.push(Inline::InlineMath { latex });
                                }
                            }
                        }
                    }
                    "strong" | "b" | "em" | "i" | "s" | "del" | "code" | "kbd" | "sup" | "sub" => {
                        *ended_with_newline = false;
                        let tag_name = el.name();
                        let mut inner = Vec::new();
                        collect_xhtml_inlines(child, ctx, blocks, &mut inner, ended_with_newline);
                        if !inner.is_empty() {
                            cur_inlines.push(Inline::Styled {
                                bold: matches!(tag_name, "strong" | "b"),
                                italic: matches!(tag_name, "em" | "i"),
                                strike: matches!(tag_name, "s" | "del"),
                                code: matches!(tag_name, "code" | "kbd"),
                                superscript: tag_name == "sup",
                                subscript: tag_name == "sub",
                                inlines: inner,
                            });
                        }
                    }
                    "a" => {
                        *ended_with_newline = false;
                        let mut inner = Vec::new();
                        collect_xhtml_inlines(child, ctx, blocks, &mut inner, ended_with_newline);
                        if !inner.is_empty() {
                            if let Some(target) =
                                el.attr("href").and_then(|href| parse_link_target(href, ctx))
                            {
                                cur_inlines.push(Inline::Link {
                                    target,
                                    inlines: inner,
                                });
                            } else {
                                cur_inlines.extend(inner);
                            }
                        }
                    }
                    "img" | "image" => {
                        if !cur_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(cur_inlines)));
                        }
                        let src = el
                            .attr("src")
                            .or_else(|| el.attr("href"))
                            .or_else(|| el.attr("xlink:href"))
                            .unwrap_or("");
                        if let Some((path, _)) = resolve_with_fragment(ctx.base_path, src) {
                            ctx.image_paths.push(path.clone());
                            blocks.push(Block::Image {
                                path,
                                alt: el.attr("alt").unwrap_or("").to_owned(),
                            });
                        }
                        *ended_with_newline = true;
                    }
                    _ => collect_xhtml_inlines(child, ctx, blocks, cur_inlines, ended_with_newline),
                }
            }
            _ => {}
        }
    }
}

fn extract_code_lang(node: NodeRef<'_, Node>) -> String {
    for candidate in std::iter::once(node).chain(node.children()) {
        if let Node::Element(el) = candidate.value()
            && let Some(cls) = el.attr("class")
        {
            for tok in cls.split_whitespace() {
                if let Some(lang) = tok.strip_prefix("language-").or_else(|| tok.strip_prefix("lang-"))
                    && !lang.is_empty()
                {
                    return lang.to_owned();
                }
            }
        }
    }
    String::new()
}

fn collect_xhtml_blocks(
    node: NodeRef<'_, Node>,
    ctx: &mut XhtmlCtx<'_>,
    blocks: &mut Vec<Block>,
    loose_inlines: &mut Vec<Inline>,
) {
    let mut ended_with_newline = false;
    for child in node.children() {
        match child.value() {
            Node::Text(t) => {
                let text = t.text.as_ref();
                if text.trim().is_empty() {
                    if text.contains('\n') && !loose_inlines.is_empty() {
                        ended_with_newline = true;
                    }
                    continue;
                }
                let mut single_inlines = Vec::new();
                let mut single_node_vec = Vec::new();
                let start_nl = ended_with_newline;
                for seg in split_math_segments(text, start_nl) {
                    match seg {
                        MathSegment::Text(s) => {
                            let c = collapse_spaces(s);
                            if !c.is_empty() {
                                push_plain_segment_with_links(&c, &mut single_inlines);
                            }
                        }
                        MathSegment::Inline(latex) => {
                            single_inlines.push(Inline::InlineMath { latex });
                        }
                        MathSegment::Display(latex) => {
                            if !single_inlines.is_empty() {
                                single_node_vec
                                    .push(Block::Paragraph(std::mem::take(&mut single_inlines)));
                            }
                            single_node_vec.push(Block::DisplayMath { latex });
                        }
                    }
                }
                loose_inlines.extend(single_inlines);
                if !single_node_vec.is_empty() {
                    if !loose_inlines.is_empty() {
                        blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                    }
                    blocks.extend(single_node_vec);
                }
                ended_with_newline = text.ends_with('\n');
            }
            Node::Element(el) => {
                let flush_block_idx = blocks.len() + usize::from(!loose_inlines.is_empty());
                record_node_anchor(child, ctx, flush_block_idx);
                let tag = el.name();
                match tag {
                    "script" | "style" | "head" | "title" => {}
                    "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let level = tag[1..].parse::<u8>().unwrap_or(1);
                        let mut inlines = Vec::new();
                        let mut sub_blocks = Vec::new();
                        let mut nl = false;
                        collect_xhtml_inlines(child, ctx, &mut sub_blocks, &mut inlines, &mut nl);
                        let anchor = el.attr("id").map(str::to_owned);
                        if !inlines.is_empty() {
                            blocks.push(Block::Heading {
                                level,
                                inlines,
                                anchor,
                            });
                        }
                        blocks.extend(sub_blocks);
                        ended_with_newline = false;
                    }
                    "p" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let mut inlines = Vec::new();
                        let mut nl = false;
                        collect_xhtml_inlines(child, ctx, blocks, &mut inlines, &mut nl);
                        if !inlines.is_empty() {
                            blocks.push(Block::Paragraph(inlines));
                        }
                        ended_with_newline = false;
                    }
                    "blockquote" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let mut inner_blocks = Vec::new();
                        let mut inner_loose = Vec::new();
                        collect_xhtml_blocks(child, ctx, &mut inner_blocks, &mut inner_loose);
                        if !inner_loose.is_empty() {
                            inner_blocks.push(Block::Paragraph(inner_loose));
                        }
                        if !inner_blocks.is_empty() {
                            blocks.push(Block::Quote(inner_blocks));
                        }
                        ended_with_newline = false;
                    }
                    "pre" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let lang = extract_code_lang(child);
                        let mut code = String::new();
                        collect_plain_element_text(child, &mut code);
                        if !code.is_empty() {
                            blocks.push(Block::Code { lang, code });
                        }
                        ended_with_newline = false;
                    }
                    "ul" | "ol" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let ordered = tag == "ol";
                        let mut items = Vec::new();
                        for li in child.children().filter(|n| {
                            matches!(n.value(), Node::Element(e) if e.name() == "li")
                        }) {
                            let mut item_blocks = Vec::new();
                            let mut item_loose = Vec::new();
                            collect_xhtml_blocks(li, ctx, &mut item_blocks, &mut item_loose);
                            if !item_loose.is_empty() {
                                item_blocks.push(Block::Paragraph(item_loose));
                            }
                            if !item_blocks.is_empty() {
                                items.push(item_blocks);
                            }
                        }
                        if !items.is_empty() {
                            blocks.push(Block::List { ordered, items });
                        }
                        ended_with_newline = false;
                    }
                    "table" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let mut headers = Vec::new();
                        let mut rows = Vec::new();
                        if let Some(table_ref) = ElementRef::wrap(child)
                            && let Ok(tr_sel) = scraper::Selector::parse("tr")
                        {
                            for tr in table_ref.select(&tr_sel) {
                                let mut row = Vec::new();
                                let mut is_header_row = false;
                                for cell_ref in tr.child_elements() {
                                    let cname = cell_ref.value().name();
                                    if !matches!(cname, "th" | "td") {
                                        continue;
                                    }
                                    if cname == "th" {
                                        is_header_row = true;
                                    }
                                    let mut cell_inlines = Vec::new();
                                    let mut cell_blocks = Vec::new();
                                    let mut nl = false;
                                    collect_xhtml_inlines(
                                        *cell_ref,
                                        ctx,
                                        &mut cell_blocks,
                                        &mut cell_inlines,
                                        &mut nl,
                                    );
                                    row.push(cell_inlines);
                                }
                                if !row.is_empty() {
                                    if is_header_row && headers.is_empty() {
                                        headers = row;
                                    } else {
                                        rows.push(row);
                                    }
                                }
                            }
                        }
                        if !headers.is_empty() || !rows.is_empty() {
                            blocks.push(Block::Table { headers, rows });
                        }
                        ended_with_newline = false;
                    }
                    "hr" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        blocks.push(Block::Rule);
                        ended_with_newline = false;
                    }
                    "div" | "section" | "article" | "figure" | "figcaption" | "aside" | "main"
                    | "header" | "footer" | "body" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let mut sub_loose = Vec::new();
                        collect_xhtml_blocks(child, ctx, blocks, &mut sub_loose);
                        if !sub_loose.is_empty() {
                            blocks.push(Block::Paragraph(sub_loose));
                        }
                        ended_with_newline = false;
                    }
                    "math" => {
                        if let Some(el_ref) = ElementRef::wrap(child) {
                            let latex = mathml_element_to_latex(el_ref);
                            if !latex.trim().is_empty() {
                                let is_sole_in_block = loose_inlines.is_empty()
                                    && !has_meaningful_following_siblings(child);
                                let is_block = el.attr("display") == Some("block")
                                    || ended_with_newline
                                    || is_sole_in_block;
                                if is_block {
                                    if !loose_inlines.is_empty() {
                                        blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                                    }
                                    blocks.push(Block::DisplayMath { latex });
                                    ended_with_newline = true;
                                } else {
                                    ended_with_newline = false;
                                    loose_inlines.push(Inline::InlineMath { latex });
                                }
                            }
                        }
                    }
                    "img" | "image" => {
                        if !loose_inlines.is_empty() {
                            blocks.push(Block::Paragraph(std::mem::take(loose_inlines)));
                        }
                        let src = el
                            .attr("src")
                            .or_else(|| el.attr("href"))
                            .or_else(|| el.attr("xlink:href"))
                            .unwrap_or("");
                        if let Some((path, _)) = resolve_with_fragment(ctx.base_path, src) {
                            ctx.image_paths.push(path.clone());
                            blocks.push(Block::Image {
                                path,
                                alt: el.attr("alt").unwrap_or("").to_owned(),
                            });
                        }
                        ended_with_newline = false;
                    }
                    _ => {
                        let mut nl = ended_with_newline;
                        let mut sub_inlines = Vec::new();
                        if tag == "ruby" {
                            collect_ruby_inlines(child, &mut sub_inlines);
                            loose_inlines.extend(sub_inlines);
                            ended_with_newline = false;
                        } else {
                            collect_xhtml_inlines(child, ctx, blocks, loose_inlines, &mut nl);
                            ended_with_newline = nl;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

fn collapse_spaces(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;
    for ch in input.chars() {
        if ch.is_whitespace() {
            if !last_space {
                out.push(' ');
                last_space = true;
            }
        } else {
            out.push(ch);
            last_space = false;
        }
    }
    out
}

#[derive(Clone)]
struct ChapterMeta {
    path: String,
    title: String,
}

fn build_chapter_index_map(chapters: &[ChapterMeta]) -> HashMap<String, usize> {
    let mut map = HashMap::new();
    for (index, chapter) in chapters.iter().enumerate() {
        map.entry(chapter.path.clone()).or_insert(index);
        if let Some(stripped) = chapter.path.strip_prefix("OEBPS/") {
            map.entry(stripped.to_owned()).or_insert(index);
        }
    }
    map
}

fn zip_entry(archive: &mut ZipArchive<fs::File>, entry: &str) -> Result<Vec<u8>, String> {
    bounded_read(
        archive
            .by_name(entry)
            .map_err(|error| format!("EPUB 资源 {entry}：{error}"))?,
    )
}

fn percent_decode(raw: &str) -> Option<String> {
    let mut result = Vec::with_capacity(raw.len());
    let mut input = raw.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            result.push((high * 16 + low) as u8);
        } else {
            result.push(byte);
        }
    }
    String::from_utf8(result).ok()
}

pub fn resolve_with_fragment(base: &str, reference: &str) -> Option<(String, String)> {
    let root = url::Url::parse("https://book.invalid/").ok()?;
    let mut base_url = root.clone();
    base_url.path_segments_mut().ok()?.extend(base.split('/'));
    let url = base_url.join(reference).ok()?;
    if url.origin() != root.origin() {
        return None;
    }
    let path = percent_decode(url.path().trim_start_matches('/'))?;
    let fragment = match url.fragment() {
        Some(frag) => percent_decode(frag)?,
        None => String::new(),
    };
    Some((path, fragment))
}

fn resolve(base: &str, reference: &str) -> Option<String> {
    resolve_with_fragment(base, reference).map(|(path, _)| path)
}

fn collect_nav_links(
    node: roxmltree::Node<'_, '_>,
    nav_path: &str,
    depth: usize,
    in_list: bool,
    spine_index: &HashMap<String, usize>,
    outline: &mut Vec<OutlineEntry>,
    first_titles: &mut HashMap<usize, String>,
) {
    let next_depth = if in_list && (node.has_tag_name("ol") || node.has_tag_name("ul")) {
        depth + 1
    } else {
        depth
    };
    let next_in_list = in_list || node.has_tag_name("ol") || node.has_tag_name("ul");
    if node.has_tag_name("a") {
        if let Some((target, fragment)) = node
            .attribute("href")
            .and_then(|href| resolve_with_fragment(nav_path, href))
        {
            let title = node
                .descendants()
                .filter(|child| child.is_text())
                .filter_map(|child| child.text())
                .collect::<Vec<_>>()
                .join(" ");
            let trimmed = title.trim();
            if !trimmed.is_empty()
                && let Some(&index) = spine_index.get(&target)
            {
                first_titles.entry(index).or_insert_with(|| trimmed.to_owned());
                outline.push(OutlineEntry {
                    title: format!("{}{}", "　".repeat(depth.min(4)), trimmed),
                    index,
                    fragment,
                });
            }
        }
        return;
    }
    for child in node.children() {
        collect_nav_links(
            child,
            nav_path,
            next_depth,
            next_in_list,
            spine_index,
            outline,
            first_titles,
        );
    }
}

fn collect_ncx_points(
    node: roxmltree::Node<'_, '_>,
    ncx_path: &str,
    depth: usize,
    spine_index: &HashMap<String, usize>,
    outline: &mut Vec<OutlineEntry>,
    first_titles: &mut HashMap<usize, String>,
) {
    for point in node.children().filter(|child| child.has_tag_name("navPoint")) {
        let title = point
            .children()
            .find(|child| child.has_tag_name("navLabel"))
            .and_then(|child| child.children().find(|text| text.has_tag_name("text")))
            .and_then(|child| child.text())
            .map(str::trim)
            .unwrap_or("");
        let target = point
            .children()
            .find(|child| child.has_tag_name("content"))
            .and_then(|child| child.attribute("src"))
            .and_then(|src| resolve_with_fragment(ncx_path, src));
        if let Some((target_path, fragment)) = target
            && !title.is_empty()
            && let Some(&index) = spine_index.get(&target_path)
        {
            first_titles.entry(index).or_insert_with(|| title.to_owned());
            outline.push(OutlineEntry {
                title: format!("{}{}", "　".repeat(depth.min(4)), title),
                index,
                fragment,
            });
        }
        collect_ncx_points(point, ncx_path, depth + 1, spine_index, outline, first_titles);
    }
}

fn collect_kindle_toc(
    points: &[ebook_rs::NavPoint],
    depth: usize,
    section_index: &HashMap<String, usize>,
    outline: &mut Vec<OutlineEntry>,
    chapters: &mut [ChapterMeta],
) {
    for point in points {
        let target = if !point.full_path.is_empty() {
            point.full_path.as_str()
        } else {
            point.href.as_str()
        };
        let (raw_path, fragment) = target.split_once('#').unwrap_or((target, ""));
        let decoded_path = percent_decode(raw_path).unwrap_or_else(|| raw_path.to_owned());
        let decoded_fragment = percent_decode(fragment).unwrap_or_else(|| fragment.to_owned());
        let found = section_index
            .get(&decoded_path)
            .or_else(|| {
                decoded_path
                    .strip_prefix("OEBPS/")
                    .and_then(|stripped| section_index.get(stripped))
            })
            .or_else(|| section_index.get(&format!("OEBPS/{decoded_path}")))
            .copied();
        let label = point.label.trim();
        if let Some(index) = found
            && !label.is_empty()
        {
            outline.push(OutlineEntry {
                title: format!("{}{}", "　".repeat(depth.min(4)), label),
                index,
                fragment: decoded_fragment,
            });
            if chapters[index].title == format!("第 {} 章", index + 1) {
                chapters[index].title = label.to_owned();
            }
        }
        collect_kindle_toc(&point.subitems, depth + 1, section_index, outline, chapters);
    }
}

fn epub_chapters(
    zip: &mut ZipArchive<fs::File>,
) -> Result<(Vec<ChapterMeta>, Vec<OutlineEntry>), String> {
    let container = zip_entry(zip, "META-INF/container.xml")?;
    let container = std::str::from_utf8(&container).map_err(|error| error.to_string())?;
    let container = roxmltree::Document::parse(container).map_err(|error| error.to_string())?;
    let package_path = container
        .descendants()
        .find(|node| node.has_tag_name("rootfile"))
        .and_then(|node| node.attribute("full-path"))
        .ok_or("EPUB 缺少内容索引")?;
    let package = zip_entry(zip, package_path)?;
    let package = std::str::from_utf8(&package).map_err(|error| error.to_string())?;
    let package = roxmltree::Document::parse(package).map_err(|error| error.to_string())?;
    let manifest = package
        .descendants()
        .find(|node| node.has_tag_name("manifest"))
        .ok_or("EPUB 缺少资源清单")?;
    let spine = package
        .descendants()
        .find(|node| node.has_tag_name("spine"))
        .ok_or("EPUB 缺少阅读顺序")?;

    let mut chapters = Vec::new();
    let mut spine_index = HashMap::new();
    for reference in spine.children().filter(|node| node.has_tag_name("itemref")) {
        let Some(id) = reference.attribute("idref") else {
            continue;
        };
        let Some(item) = manifest
            .children()
            .find(|item| item.attribute("id") == Some(id))
        else {
            continue;
        };
        if !matches!(
            item.attribute("media-type"),
            Some("application/xhtml+xml" | "text/html")
        ) {
            continue;
        }
        let Some(entry) = item
            .attribute("href")
            .and_then(|href| resolve(package_path, href))
        else {
            continue;
        };
        let index = chapters.len();
        spine_index.entry(entry.clone()).or_insert(index);
        chapters.push(ChapterMeta {
            path: entry,
            title: format!("第 {} 章", index + 1),
        });
    }
    if chapters.is_empty() {
        return Err("EPUB 没有可读的正文，文件可能受保护".into());
    }

    let mut outline = Vec::new();
    let mut first_titles = HashMap::new();
    if let Some(nav_path) = manifest
        .children()
        .find(|node| {
            node.attribute("properties")
                .is_some_and(|value| value.split_whitespace().any(|property| property == "nav"))
        })
        .and_then(|node| resolve(package_path, node.attribute("href")?))
        && let Ok(bytes) = zip_entry(zip, &nav_path)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Ok(nav) = roxmltree::Document::parse(text)
    {
        let toc_node = nav
            .descendants()
            .find(|node| {
                node.has_tag_name("nav")
                    && node
                        .attributes()
                        .any(|attr| attr.name() == "type" && attr.value() == "toc")
            })
            .or_else(|| nav.descendants().find(|node| node.has_tag_name("nav")))
            .unwrap_or_else(|| nav.root());
        collect_nav_links(
            toc_node,
            &nav_path,
            0,
            false,
            &spine_index,
            &mut outline,
            &mut first_titles,
        );
    }
    if outline.is_empty()
        && let Some(ncx_path) = spine
            .attribute("toc")
            .and_then(|id| {
                manifest
                    .children()
                    .find(|item| item.attribute("id") == Some(id))
            })
            .and_then(|node| resolve(package_path, node.attribute("href")?))
        && let Ok(bytes) = zip_entry(zip, &ncx_path)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Ok(ncx) = roxmltree::Document::parse(text)
        && let Some(nav_map) = ncx.descendants().find(|node| node.has_tag_name("navMap"))
    {
        collect_ncx_points(
            nav_map,
            &ncx_path,
            0,
            &spine_index,
            &mut outline,
            &mut first_titles,
        );
    }
    for (index, title) in first_titles {
        chapters[index].title = title;
    }
    Ok((chapters, outline))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;

    #[test]
    fn decodes_utf8_utf16_and_gb18030() {
        assert_eq!(decode("你好世界".as_bytes(), None).unwrap(), "你好世界");
        let utf16le: Vec<u8> = [0xFFu8, 0xFE, b'H', 0, b'i', 0].to_vec();
        assert_eq!(decode(&utf16le, None).unwrap(), "Hi");
        let utf16be: Vec<u8> = [0xFEu8, 0xFF, 0, b'H', 0, b'i'].to_vec();
        assert_eq!(decode(&utf16be, None).unwrap(), "Hi");
        let (gb, _, _) = GB18030.encode("中文测试");
        assert_eq!(decode(&gb, Some("gb18030")).unwrap(), "中文测试");
        assert!(decode(b"abc", Some("unknown-enc")).is_err());
    }

    #[test]
    fn resolves_relative_paths_fragments_and_blocks_external() {
        assert_eq!(
            resolve_with_fragment("OEBPS/content.opf", "Text/ch%201.xhtml#sec-2"),
            Some(("OEBPS/Text/ch 1.xhtml".to_owned(), "sec-2".to_owned()))
        );
        assert_eq!(
            resolve_with_fragment("OEBPS/nav.xhtml", "../Images/%E5%9B%BE.png"),
            Some(("Images/图.png".to_owned(), String::new()))
        );
        assert_eq!(
            resolve_with_fragment("OEBPS/nav.xhtml", "https://example.com/a.xhtml"),
            None
        );
    }

    #[test]
    fn distinguishes_inline_vs_display_formulas_by_preceding_newline() {
        // Inline even with $$ when NOT preceded by newline:
        let md = "设公式 $E=mc^2$ 和 $$a^2+b^2=c^2$$ 均在文中。\n\n单独一行的公式：\n$$\\frac{1}{2}$$\n正文结束。";
        let doc = parse_markdown_document(md, None);
        let blocks = &doc.chapters[0].blocks;
        assert_eq!(
            blocks[0],
            Block::Paragraph(vec![
                Inline::Text("设公式 ".to_owned()),
                Inline::InlineMath {
                    latex: "E=mc^2".to_owned()
                },
                Inline::Text(" 和 ".to_owned()),
                Inline::InlineMath {
                    latex: "a^2+b^2=c^2".to_owned()
                },
                Inline::Text(" 均在文中。".to_owned()),
            ])
        );
        assert_eq!(
            blocks[1],
            Block::Paragraph(vec![Inline::Text("单独一行的公式：".to_owned())])
        );
        assert_eq!(
            blocks[2],
            Block::DisplayMath {
                latex: r"\frac{1}{2}".to_owned()
            }
        );
        assert_eq!(
            blocks[3],
            Block::Paragraph(vec![Inline::Text("正文结束。".to_owned())])
        );
    }

    #[test]
    fn parses_ruby_furigana_and_mathml_from_xhtml() {
        let xhtml = r#"<html xmlns="http://www.w3.org/1999/xhtml"><body>
<p id="p1"><ruby><rb>異世界</rb><rp>(</rp><rt>いせかい</rt><rp>)</rp></ruby>へようこそ！行内数式：<math><msup><mi>x</mi><mn>2</mn></msup></math>。</p>
<p id="p2">单独公式：
<math><mfrac><mn>1</mn><mn>3</mn></mfrac></math></p>
</body></html>"#;
        let (blocks, _, anchors) =
            parse_xhtml_blocks_with_anchors(xhtml, "OEBPS/ch1.xhtml", 0, &HashMap::new());
        assert_eq!(anchors.get("p1").copied(), Some(0));
        assert_eq!(anchors.get("p2").copied(), Some(1));
        assert_eq!(
            blocks[0],
            Block::Paragraph(vec![
                Inline::Ruby {
                    base: "異世界".to_owned(),
                    annotation: "いせかい".to_owned()
                },
                Inline::Text("へようこそ！行内数式：".to_owned()),
                Inline::InlineMath {
                    latex: "x^{2}".to_owned()
                },
                Inline::Text("。".to_owned()),
            ])
        );
        assert_eq!(
            blocks[1],
            Block::Paragraph(vec![Inline::Text("单独公式： ".to_owned())])
        );
        assert_eq!(
            blocks[2],
            Block::DisplayMath {
                latex: r"\frac{1}{3}".to_owned()
            }
        );
    }

    #[test]
    fn parses_html5_clean_mode_and_xml_code_mode() {
        let dir = tempfile::tempdir().unwrap();
        let html_path = dir.path().join("article.html");
        fs::write(
            &html_path,
            "<!doctype html><html><head><title>测试页</title></head><body><h1>第一章</h1><p>A &times; B<br>欢迎阅读</p></body></html>",
        )
        .unwrap();
        let hdoc = open_html(&html_path).unwrap();
        assert_eq!(hdoc.kind, "html");
        assert_eq!(hdoc.chapters[0].title, "测试页");
        assert!(!hdoc.chapters[0].raw_source.is_empty());
        assert_eq!(hdoc.outline.len(), 1);

        let xml_path = dir.path().join("data.xml");
        fs::write(&xml_path, "<root><item id=\"1\">Hello XML</item></root>").unwrap();
        let xdoc = open_xml(&xml_path).unwrap();
        assert_eq!(xdoc.kind, "xml");
        assert!(xdoc.chapters[0].raw_source.contains("<item"));
    }

    #[test]
    fn parses_synthetic_epub_with_nested_outline_and_ruby() {
        let dir = tempfile::tempdir().unwrap();
        let epub_path = dir.path().join("sample.epub");
        {
            let file = fs::File::create(&epub_path).unwrap();
            let mut writer = zip::ZipWriter::new(file);
            let opts = SimpleFileOptions::default();
            writer.start_file("META-INF/container.xml", opts).unwrap();
            writer
                .write_all(br#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/package.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#)
                .unwrap();
            writer.start_file("OEBPS/package.opf", opts).unwrap();
            writer
                .write_all(br#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="c1" href="ch1.xhtml" media-type="application/xhtml+xml"/></manifest><spine><itemref idref="c1"/></spine></package>"#)
                .unwrap();
            writer.start_file("OEBPS/nav.xhtml", opts).unwrap();
            writer
                .write_all(br#"<?xml version="1.0"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><body><nav epub:type="toc"><ol><li><a href="ch1.xhtml#top">Chapter 1</a><ol><li><a href="ch1.xhtml#sub">Section 1.1</a></li></ol></li></ol></nav></body></html>"#)
                .unwrap();
            writer.start_file("OEBPS/ch1.xhtml", opts).unwrap();
            writer
                .write_all(r#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="top">Chapter 1</h1><p><ruby>漢字<rt>かんじ</rt></ruby></p></body></html>"#.as_bytes())
                .unwrap();
            writer.finish().unwrap();
        }
        let doc = open_epub(&epub_path).unwrap();
        assert_eq!(doc.chapters.len(), 1);
        assert_eq!(
            doc.outline,
            vec![
                OutlineEntry {
                    title: "Chapter 1".into(),
                    index: 0,
                    fragment: "top".into()
                },
                OutlineEntry {
                    title: "　Section 1.1".into(),
                    index: 0,
                    fragment: "sub".into()
                },
            ]
        );
        assert_eq!(
            doc.chapters[0].blocks[1],
            Block::Paragraph(vec![Inline::Ruby {
                base: "漢字".into(),
                annotation: "かんじ".into()
            }])
        );
    }
}
