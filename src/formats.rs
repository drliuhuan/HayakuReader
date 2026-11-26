use encoding_rs::{GB18030, UTF_8, UTF_16BE, UTF_16LE};
use serde::Serialize;
use std::{
    collections::HashMap,
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Mutex,
};
use zip::ZipArchive;

pub const EXTENSIONS: &[&str] = &[
    "pdf", "epub", "txt", "azw3", "azw", "mobi", "doc", "docx", "html", "htm", "md", "markdown",
];
const MAX_RESOURCE: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Chapter {
    pub path: String,
    pub title: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct OutlineEntry {
    pub title: String,
    pub index: usize,
    pub fragment: String,
}

pub enum Source {
    Pdf(PathBuf),
    Text(PathBuf),
    Html(PathBuf),
    Markdown(PathBuf),
    Epub(Mutex<ZipArchive<fs::File>>, Vec<Chapter>, Vec<OutlineEntry>),
    Kindle(Box<ebook_rs::Book>, Vec<Chapter>, Vec<OutlineEntry>),
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
        "epub" => {
            let file = fs::File::open(path).map_err(|error| error.to_string())?;
            let mut zip = ZipArchive::new(file).map_err(|error| error.to_string())?;
            let (chapters, outline) = chapters(&mut zip)?;
            Source::Epub(Mutex::new(zip), chapters, outline)
        }
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
            let mut chapters = book
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
            let mut section_index = HashMap::new();
            for (index, chapter) in chapters.iter().enumerate() {
                section_index.entry(chapter.path.clone()).or_insert(index);
                if let Some(stripped) = chapter.path.strip_prefix("OEBPS/") {
                    section_index.entry(stripped.to_owned()).or_insert(index);
                }
            }
            let mut outline = Vec::new();
            collect_kindle_toc(book.toc(), 0, &section_index, &mut outline, &mut chapters);
            Source::Kindle(Box::new(book), chapters, outline)
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

fn collect_kindle_toc(
    points: &[ebook_rs::NavPoint],
    depth: usize,
    section_index: &HashMap<String, usize>,
    outline: &mut Vec<OutlineEntry>,
    chapters: &mut [Chapter],
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

pub fn zip_entry(archive: &mut ZipArchive<fs::File>, entry: &str) -> Result<Vec<u8>, String> {
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
    // ZIP entry names are decoded UTF-8 paths, including spaces and non-ASCII names.
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

fn chapters(zip: &mut ZipArchive<fs::File>) -> Result<(Vec<Chapter>, Vec<OutlineEntry>), String> {
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
        chapters.push(Chapter {
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
    fn renders_markdown_and_escapes_raw_html() {
        let html = render_markdown("# 标题\n\n<script>alert(1)</script>\n\n~~old~~ [x] task\n\n| A | B |\n| - | - |\n| 1 | 2 |\n");
        assert!(html.contains("<h1>标题</h1>"));
        assert!(!html.contains("<script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(html.contains("<del>old</del>"));
        assert!(html.contains("<table>"));
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
        assert_eq!(resolve_with_fragment("OEBPS/nav.xhtml", "https://example.com/a.xhtml"), None);
    }

    #[test]
    fn parses_synthetic_epub_with_nested_outline_and_fragments() {
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
                .write_all(br#"<html xmlns="http://www.w3.org/1999/xhtml"><body><h1 id="top">Chapter 1</h1><h2 id="sub">Section 1.1</h2></body></html>"#)
                .unwrap();
            writer.finish().unwrap();
        }
        let Source::Epub(zip, chapters, outline) = load(&epub_path).unwrap() else {
            panic!("expected epub source");
        };
        assert_eq!(chapters, vec![Chapter { path: "OEBPS/ch1.xhtml".into(), title: "Chapter 1".into() }]);
        assert_eq!(
            outline,
            vec![
                OutlineEntry { title: "Chapter 1".into(), index: 0, fragment: "top".into() },
                OutlineEntry { title: "　Section 1.1".into(), index: 0, fragment: "sub".into() },
            ]
        );
        let mut guard = zip.lock().unwrap();
        let ch1 = zip_entry(&mut guard, "OEBPS/ch1.xhtml").unwrap();
        assert!(std::str::from_utf8(&ch1).unwrap().contains("Section 1.1"));
    }
}
