use encoding_rs::{GB18030, UTF_8, UTF_16BE, UTF_16LE};
use serde::Serialize;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
};
use zip::ZipArchive;

pub const EXTENSIONS: &[&str] = &[
    "pdf", "epub", "txt", "azw3", "azw", "mobi", "doc", "docx", "html", "htm", "md", "markdown",
];
const MAX_RESOURCE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Serialize)]
pub struct Chapter {
    pub path: String,
    pub title: String,
}

pub enum Source {
    Pdf(PathBuf),
    Text(PathBuf),
    Html(PathBuf),
    Markdown(PathBuf),
    Epub(PathBuf, Vec<Chapter>),
    Kindle(Box<ebook_rs::Book>, Vec<Chapter>),
    Word(crate::word::Word),
}

pub fn load(path: &Path) -> Result<Source, String> {
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let source = match extension.as_str() {
        "pdf" => Source::Pdf(path.to_owned()),
        "txt" => Source::Text(path.to_owned()),
        "html" | "htm" => Source::Html(path.to_owned()),
        "md" | "markdown" => Source::Markdown(path.to_owned()),
        "epub" => Source::Epub(path.to_owned(), chapters(path)?),
        "doc" | "docx" => Source::Word(crate::word::Word::open(path)?),
        "azw" | "azw3" | "mobi" => {
            let data = read_bytes(path)?;
            let book = ebook_rs::MobiBook::parse(&data).map_err(|error| match error {
                ebook_rs::EbookError::DrmProtected(_) => "此电子书受 DRM 保护".to_owned(),
                other if other.to_string().contains("HUFF/CDIC") => {
                    "暂不支持使用 HUFF/CDIC 压缩的 Kindle 文件".to_owned()
                }
                other => format!("无法解析 Kindle 文件：{other}"),
            })?;
            let chapters = book
                .sections()
                .iter()
                .enumerate()
                .map(|(index, section)| Chapter {
                    path: section.full_path.clone(),
                    title: format!("第 {} 章", index + 1),
                })
                .collect::<Vec<_>>();
            if chapters.is_empty() {
                return Err("Kindle 文件没有可读章节".into());
            }
            Source::Kindle(Box::new(book), chapters)
        }
        _ => return Err("不支持此文件格式".into()),
    };
    if let Source::Pdf(pdf) = &source {
        let mut header = [0; 1024];
        let count = fs::File::open(pdf)
            .and_then(|mut file| file.read(&mut header))
            .map_err(|error| error.to_string())?;
        if !header[..count].windows(5).any(|bytes| bytes == b"%PDF-") {
            return Err("PDF 文件头无效".into());
        }
    }
    Ok(source)
}

pub fn read_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    bounded_read(file)
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

pub fn markdown(path: &Path) -> Result<String, String> {
    let source = decode(&read_bytes(path)?, None)?;
    Ok(render_markdown(&source))
}

pub fn render_markdown(source: &str) -> String {
    use pulldown_cmark::{Event, Options, Parser, html};
    let options =
        Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let events = Parser::new_ext(source, options).map(|event| match event {
        Event::Html(text) | Event::InlineHtml(text) => Event::Text(text),
        other => other,
    });
    let mut markup = String::new();
    html::push_html(&mut markup, events);
    markup
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

pub fn archive_entry(path: &Path, entry: &str) -> Result<Vec<u8>, String> {
    let file = fs::File::open(path).map_err(|error| error.to_string())?;
    let mut zip = ZipArchive::new(file).map_err(|error| error.to_string())?;
    bounded_read(
        zip.by_name(entry)
            .map_err(|error| format!("EPUB 资源 {entry}：{error}"))?,
    )
}

fn resolve(base: &str, reference: &str) -> Option<String> {
    let root = url::Url::parse("https://book.invalid/").ok()?;
    let mut base_url = root.clone();
    base_url.path_segments_mut().ok()?.extend(base.split('/'));
    let url = base_url.join(reference).ok()?;
    if url.origin() != root.origin() {
        return None;
    }
    // ZIP entry names are decoded UTF-8 paths, including spaces and non-ASCII names.
    let mut result = Vec::new();
    let path = url.path().trim_start_matches('/').as_bytes();
    let mut input = path.iter().copied();
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

fn chapters(path: &Path) -> Result<Vec<Chapter>, String> {
    let container = archive_entry(path, "META-INF/container.xml")?;
    let container = std::str::from_utf8(&container).map_err(|error| error.to_string())?;
    let container = roxmltree::Document::parse(container).map_err(|error| error.to_string())?;
    let package_path = container
        .descendants()
        .find(|node| node.has_tag_name("rootfile"))
        .and_then(|node| node.attribute("full-path"))
        .ok_or("EPUB 缺少内容索引")?;
    let package = archive_entry(path, package_path)?;
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
    let mut titles = std::collections::HashMap::new();
    if let Some(nav_path) = manifest
        .children()
        .find(|node| {
            node.attribute("properties")
                .is_some_and(|value| value.split_whitespace().any(|property| property == "nav"))
        })
        .and_then(|node| resolve(package_path, node.attribute("href")?))
        && let Ok(bytes) = archive_entry(path, &nav_path)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Ok(nav) = roxmltree::Document::parse(text)
    {
        for link in nav.descendants().filter(|node| node.has_tag_name("a")) {
            if let Some(target) = link
                .attribute("href")
                .and_then(|href| resolve(&nav_path, href))
            {
                let title = link
                    .descendants()
                    .filter(|node| node.is_text())
                    .filter_map(|node| node.text())
                    .collect::<Vec<_>>()
                    .join(" ");
                if !title.trim().is_empty() {
                    titles.entry(target).or_insert(title);
                }
            }
        }
    }
    if titles.is_empty()
        && let Some(ncx_path) = spine
            .attribute("toc")
            .and_then(|id| {
                manifest
                    .children()
                    .find(|item| item.attribute("id") == Some(id))
            })
            .and_then(|node| resolve(package_path, node.attribute("href")?))
        && let Ok(bytes) = archive_entry(path, &ncx_path)
        && let Ok(text) = std::str::from_utf8(&bytes)
        && let Ok(ncx) = roxmltree::Document::parse(text)
    {
        for point in ncx
            .descendants()
            .filter(|node| node.has_tag_name("navPoint"))
        {
            let title = point
                .children()
                .find(|node| node.has_tag_name("navLabel"))
                .and_then(|node| node.children().find(|child| child.has_tag_name("text")))
                .and_then(|node| node.text());
            let target = point
                .children()
                .find(|node| node.has_tag_name("content"))
                .and_then(|node| node.attribute("src"))
                .and_then(|src| resolve(&ncx_path, src));
            if let (Some(target), Some(title)) = (target, title) {
                titles.entry(target).or_insert_with(|| title.to_owned());
            }
        }
    }
    let mut chapters = Vec::new();
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
        let title = titles
            .get(&entry)
            .cloned()
            .unwrap_or_else(|| format!("第 {} 章", chapters.len() + 1));
        chapters.push(Chapter { path: entry, title });
    }
    if chapters.is_empty() {
        return Err("EPUB 没有可读的正文，文件可能受保护".into());
    }
    Ok(chapters)
}
