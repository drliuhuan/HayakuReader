use crate::flow::OutlineEntry;
use flate2::read::GzDecoder;
use pdfium_render::prelude::*;
#[cfg(not(embedded_pdfium))]
use std::io::Read;
use std::{
    fs,
    io::{self, Cursor},
    path::{Path, PathBuf},
    sync::OnceLock,
};
#[cfg(not(embedded_pdfium))]
use tar::Archive;

#[cfg(not(embedded_pdfium))]
const PDFIUM_WIN_X64_URL: &str =
    "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium%2F8066/pdfium-win-x64.tgz";

pub struct PdfLinkItem {
    /// Rect in page points, top-left origin: [left, top, width, height]
    pub rect_pt: [f32; 4],
    pub uri: Option<String>,
    pub dest_page: Option<usize>,
}

#[derive(Clone, Debug)]
pub struct PdfTextRun {
    pub text: String,
    /// Rect in page points, top-left origin: [left, top, width, height]
    pub rect_pt: [f32; 4],
}

pub struct RenderedPdfPage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
    /// Embedded image bounding rects in rendered pixel coordinates [x, y, w, h]
    pub image_rects_px: Vec<[f32; 4]>,
}

pub struct PdfDoc {
    doc: PdfDocument<'static>,
    page_sizes: Vec<[f32; 2]>,
    outline: Vec<OutlineEntry>,
}

#[cfg(embedded_pdfium)]
const EMBEDDED_PDFIUM_GZ: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pdfium.dll.gz"));
#[cfg(embedded_pdfium)]
const EMBEDDED_PDFIUM_LEN: &str = include_str!(concat!(env!("OUT_DIR"), "/pdfium.len"));

fn pdfium_cache_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(|p| PathBuf::from(p).join(".cache")))
        .unwrap_or_else(std::env::temp_dir)
        .join("HayakuReader")
        .join("pdfium")
        .join("pdfium-8066")
}

#[cfg(embedded_pdfium)]
fn ensure_embedded_pdfium_dll() -> Result<PathBuf, String> {
    let lib_name = Pdfium::pdfium_platform_library_name();
    let dir = pdfium_cache_dir();
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建内置 PDFium 缓存目录：{e}"))?;
    let dest = dir.join(&lib_name);
    let expected_len: u64 = EMBEDDED_PDFIUM_LEN.trim().parse().unwrap_or(0);
    if dest.is_file()
        && expected_len > 0
        && fs::metadata(&dest).is_ok_and(|m| m.len() == expected_len)
    {
        return Ok(dest);
    }
    let tmp = dir.join(format!("{}.{}.tmp", lib_name.to_string_lossy(), std::process::id()));
    {
        let mut decoder = GzDecoder::new(Cursor::new(EMBEDDED_PDFIUM_GZ));
        let mut out = fs::File::create(&tmp)
            .map_err(|e| format!("写出内置 pdfium.dll 失败：{e}"))?;
        io::copy(&mut decoder, &mut out)
            .map_err(|e| format!("解压内置 pdfium.dll 失败：{e}"))?;
    }
    if dest.is_file() {
        let _ = fs::remove_file(&dest);
    }
    if let Err(e) = fs::rename(&tmp, &dest) {
        let _ = fs::remove_file(&tmp);
        if dest.is_file() {
            return Ok(dest);
        }
        return Err(format!("安装内置 pdfium.dll 失败：{e}"));
    }
    Ok(dest)
}

#[cfg(any(not(embedded_pdfium), test))]
fn find_cached_pdfium_dll() -> Option<PathBuf> {
    let lib_name = Pdfium::pdfium_platform_library_name();
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(&lib_name));
    }
    if let Some(env_path) = std::env::var_os("PDFIUM_LIB_PATH") {
        candidates.push(PathBuf::from(env_path));
    }
    let local_root = pdfium_cache_dir();
    candidates.push(local_root.join(&lib_name));
    if let Some(parent) = local_root.parent() {
        candidates.push(parent.join(&lib_name));
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        let cache_root = PathBuf::from(profile).join(".cache").join("pdfium-bundled");
        candidates.push(cache_root.join("pdfium-8066").join(&lib_name));
        if let Ok(entries) = fs::read_dir(&cache_root) {
            for entry in entries.flatten() {
                candidates.push(entry.path().join(&lib_name));
            }
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(not(embedded_pdfium))]
fn download_pdfium_dll() -> Result<PathBuf, String> {
    let lib_name = Pdfium::pdfium_platform_library_name();
    let base_dir = pdfium_cache_dir();
    fs::create_dir_all(&base_dir).map_err(|e| format!("无法创建 PDFium 缓存目录：{e}"))?;
    let dest = base_dir.join(&lib_name);
    if dest.is_file() {
        return Ok(dest);
    }

    let response = ureq::get(PDFIUM_WIN_X64_URL)
        .call()
        .map_err(|e| format!("无法下载 PDFium 运行库：{e}"))?;
    let mut tgz_bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut tgz_bytes)
        .map_err(|e| format!("读取 PDFium 下载流失败：{e}"))?;

    let decoder = GzDecoder::new(Cursor::new(tgz_bytes));
    let mut archive = Archive::new(decoder);
    let tmp = base_dir.join(format!("{}.tmp", lib_name.to_string_lossy()));
    let mut extracted = false;
    for entry_res in archive
        .entries()
        .map_err(|e| format!("解压 PDFium 压缩包失败：{e}"))?
    {
        let mut entry = entry_res.map_err(|e| format!("读取 PDFium 压缩包条目失败：{e}"))?;
        let Ok(path) = entry.path() else {
            continue;
        };
        if path.file_name() == Some(lib_name.as_os_str()) {
            let mut out =
                fs::File::create(&tmp).map_err(|e| format!("写入 pdfium.dll 失败：{e}"))?;
            io::copy(&mut entry, &mut out).map_err(|e| format!("解压 pdfium.dll 失败：{e}"))?;
            extracted = true;
            break;
        }
    }
    if !extracted {
        let _ = fs::remove_file(&tmp);
        return Err("PDFium 压缩包中未找到 pdfium.dll".to_owned());
    }
    fs::rename(&tmp, &dest).map_err(|e| format!("保存 pdfium.dll 失败：{e}"))?;
    Ok(dest)
}

fn pdfium_instance() -> Result<&'static Pdfium, String> {
    static PDFIUM: OnceLock<Result<Pdfium, String>> = OnceLock::new();
    let res = PDFIUM.get_or_init(|| {
        let exe_local = std::env::current_exe()
            .ok()
            .and_then(|e| e.parent().map(|d| d.join(Pdfium::pdfium_platform_library_name())))
            .filter(|p| p.is_file());
        let lib_path = if let Some(found) = exe_local {
            found
        } else {
            #[cfg(embedded_pdfium)]
            {
                ensure_embedded_pdfium_dll()?
            }
            #[cfg(not(embedded_pdfium))]
            {
                if let Some(found) = find_cached_pdfium_dll() {
                    found
                } else {
                    download_pdfium_dll()?
                }
            }
        };
        let bindings = Pdfium::bind_to_library(&lib_path)
            .or_else(|_| Pdfium::bind_to_system_library())
            .map_err(|e| {
                format!(
                    "无法初始化 PDF 引擎 (PDFium) ({})：{e}",
                    lib_path.display()
                )
            })?;
        Ok(Pdfium::new(bindings))
    });
    res.as_ref().map_err(Clone::clone)
}

fn resolve_bookmark_page(bm: &PdfBookmark<'_>) -> Option<usize> {
    if let Some(dest) = bm.destination()
        && let Ok(idx) = dest.page_index()
    {
        return Some(idx as usize);
    }
    if let Some(action) = bm.action()
        && let Some(local) = action.as_local_destination_action()
        && let Ok(dest) = local.destination()
        && let Ok(idx) = dest.page_index()
    {
        return Some(idx as usize);
    }
    None
}

fn collect_bookmarks(bm: Option<PdfBookmark<'_>>, depth: usize, out: &mut Vec<OutlineEntry>) {
    let mut current = bm;
    while let Some(node) = current {
        if let Some(title) = node.title() {
            let trimmed = title.trim();
            if !trimmed.is_empty()
                && let Some(index) = resolve_bookmark_page(&node)
            {
                let indent = "　".repeat(depth.min(4));
                out.push(OutlineEntry {
                    title: format!("{indent}{trimmed}"),
                    index,
                    fragment: String::new(),
                });
            }
        }
        collect_bookmarks(node.first_child(), depth + 1, out);
        current = node.next_sibling();
    }
}

fn collect_image_boxes_in_points(page: &PdfPage<'_>, page_h: f32) -> Vec<[f32; 4]> {
    let mut boxes = Vec::new();
    for obj in page.objects().iter() {
        if obj.object_type() == PdfPageObjectType::Image
            && let Ok(bounds) = obj.bounds()
        {
            let left = bounds.left().value;
            let top = (page_h - bounds.top().value).max(0.0);
            let width = (bounds.right().value - bounds.left().value).abs();
            let height = (bounds.top().value - bounds.bottom().value).abs();
            if width > 1.0 && height > 1.0 {
                boxes.push([left, top, width, height]);
            }
        }
    }
    boxes
}

impl PdfDoc {
    pub fn open(path: &Path) -> Result<Self, String> {
        let pdfium = pdfium_instance()?;
        let doc = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| format!("无法打开 PDF 文件：{e}"))?;
        let mut page_sizes = Vec::with_capacity(doc.pages().len() as usize);
        for page in doc.pages().iter() {
            page_sizes.push([page.width().value.max(1.0), page.height().value.max(1.0)]);
        }
        if page_sizes.is_empty() {
            return Err("PDF 文件不包含任何页面".to_owned());
        }
        let mut outline = Vec::new();
        collect_bookmarks(doc.bookmarks().root(), 0, &mut outline);
        Ok(Self {
            doc,
            page_sizes,
            outline,
        })
    }

    pub fn page_count(&self) -> usize {
        self.page_sizes.len()
    }

    pub fn page_sizes(&self) -> &[[f32; 2]] {
        &self.page_sizes
    }

    pub fn outline(&self) -> &[OutlineEntry] {
        &self.outline
    }

    pub fn render_rgba(&self, index: usize, requested_scale: f32) -> Result<RenderedPdfPage, String> {
        let page = self
            .doc
            .pages()
            .get(i32::try_from(index).map_err(|_| "页码超出范围")?)
            .map_err(|e| e.to_string())?;
        let pw = page.width().value.max(1.0);
        let ph = page.height().value.max(1.0);
        let max_scale = (16_000_000.0 / (pw * ph)).sqrt().clamp(0.5, 5.0);
        let scale = requested_scale.clamp(0.35, max_scale);
        let target_w = (pw * scale).ceil().max(1.0) as i32;
        let target_h = (ph * scale).ceil().max(1.0) as i32;
        let config = PdfRenderConfig::new()
            .set_target_size(target_w, target_h)
            .render_annotations(true)
            .render_form_data(true);
        let bitmap = page
            .render_with_config(&config)
            .map_err(|e| format!("渲染 PDF 第 {} 页失败：{e}", index + 1))?;
        let width = bitmap.width() as u32;
        let height = bitmap.height() as u32;
        let rgba = bitmap.as_rgba_bytes();
        let sx = width as f32 / pw;
        let sy = height as f32 / ph;
        let image_rects_px = collect_image_boxes_in_points(&page, ph)
            .into_iter()
            .map(|[x, y, w, h]| [x * sx, y * sy, w * sx, h * sy])
            .collect();
        Ok(RenderedPdfPage {
            width,
            height,
            rgba,
            image_rects_px,
        })
    }

    pub fn extract_image_png(
        &self,
        index: usize,
        page_x_pt: f32,
        page_y_pt: f32,
    ) -> Result<Option<Vec<u8>>, String> {
        let page = self
            .doc
            .pages()
            .get(i32::try_from(index).map_err(|_| "页码超出范围")?)
            .map_err(|e| e.to_string())?;
        let ph = page.height().value.max(1.0);
        let mut best_area = f32::INFINITY;
        let mut best_png: Option<Vec<u8>> = None;
        for obj in page.objects().iter() {
            if obj.object_type() != PdfPageObjectType::Image {
                continue;
            }
            let Ok(bounds) = obj.bounds() else {
                continue;
            };
            let left = bounds.left().value;
            let top = ph - bounds.top().value;
            let w = (bounds.right().value - bounds.left().value).abs();
            let h = (bounds.top().value - bounds.bottom().value).abs();
            if w < 8.0 || h < 8.0 {
                continue;
            }
            if page_x_pt >= left
                && page_x_pt <= left + w
                && page_y_pt >= top
                && page_y_pt <= top + h
            {
                let area = w * h;
                if area <= best_area
                    && let Some(img_obj) = obj.as_image_object()
                    && let Ok(bitmap) = img_obj
                        .get_processed_bitmap(&self.doc)
                        .or_else(|_| img_obj.get_raw_bitmap())
                    && let Ok(dyn_img) = bitmap.as_image()
                {
                    let mut buf = Cursor::new(Vec::new());
                    if dyn_img.write_to(&mut buf, image::ImageFormat::Png).is_ok() {
                        best_area = area;
                        best_png = Some(buf.into_inner());
                    }
                }
            }
        }
        Ok(best_png)
    }

    pub fn page_links(&self, index: usize) -> Vec<PdfLinkItem> {
        let Ok(idx) = i32::try_from(index) else {
            return Vec::new();
        };
        let Ok(page) = self.doc.pages().get(idx) else {
            return Vec::new();
        };
        let ph = page.height().value.max(1.0);
        let mut out = Vec::new();
        for link in page.links().iter() {
            let Ok(rect) = link.rect() else {
                continue;
            };
            let left = rect.left().value;
            let top = (ph - rect.top().value).max(0.0);
            let width = (rect.right().value - rect.left().value).abs();
            let height = (rect.top().value - rect.bottom().value).abs();
            if width < 2.0 || height < 2.0 {
                continue;
            }
            let mut uri = None;
            let mut dest_page = None;
            if let Some(action) = link.action() {
                if let Some(uri_act) = action.as_uri_action()
                    && let Ok(u) = uri_act.uri()
                    && !u.trim().is_empty()
                {
                    uri = Some(u);
                } else if let Some(local) = action.as_local_destination_action()
                    && let Ok(dest) = local.destination()
                    && let Ok(p) = dest.page_index()
                {
                    dest_page = Some(p as usize);
                }
            }
            if uri.is_none()
                && dest_page.is_none()
                && let Some(dest) = link.destination()
                && let Ok(p) = dest.page_index()
            {
                dest_page = Some(p as usize);
            }
            if uri.is_some() || dest_page.is_some() {
                out.push(PdfLinkItem {
                    rect_pt: [left, top, width, height],
                    uri,
                    dest_page,
                });
            }
        }
        out
    }

    pub fn page_text_runs(&self, index: usize) -> Vec<PdfTextRun> {
        let Ok(idx) = i32::try_from(index) else {
            return Vec::new();
        };
        let Ok(page) = self.doc.pages().get(idx) else {
            return Vec::new();
        };
        let ph = page.height().value.max(1.0);
        let Ok(text_page) = page.text() else {
            return Vec::new();
        };
        let mut runs = Vec::new();
        for seg in text_page.segments().iter() {
            let bounds = seg.bounds();
            let left = bounds.left().value;
            let top = (ph - bounds.top().value).max(0.0);
            let width = (bounds.right().value - bounds.left().value).abs();
            let height = (bounds.top().value - bounds.bottom().value).abs();
            if width < 0.5 || height < 0.5 {
                continue;
            }
            let text = seg
                .text()
                .trim_matches(|c| c == '\r' || c == '\n' || c == '\0')
                .to_owned();
            if text.is_empty() {
                continue;
            }
            runs.push(PdfTextRun {
                text,
                rect_pt: [left, top, width, height],
            });
        }
        runs
    }

    pub fn page_text(&self, index: usize) -> String {
        let Ok(idx) = i32::try_from(index) else {
            return String::new();
        };
        let Ok(page) = self.doc.pages().get(idx) else {
            return String::new();
        };
        page.text().map(|t| t.all()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binds_cached_pdfium_library_if_present() {
        if find_cached_pdfium_dll().is_some() {
            assert!(
                pdfium_instance().is_ok(),
                "expected cached pdfium.dll to bind cleanly without GetProcAddress error"
            );
        }
    }
}
