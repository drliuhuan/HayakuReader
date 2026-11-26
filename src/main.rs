#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop;
mod formats;
#[cfg(windows)]
mod platform;
mod word;

use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{DefaultBodyLimit, Path as RoutePath, Query, State},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use formats::{Chapter, OutlineEntry, Source};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Clone, Serialize, Deserialize)]
struct Recent {
    id: String,
    path: PathBuf,
    digest: String,
}

#[derive(Serialize)]
struct Document {
    session: String,
    id: String,
    digest: String,
    title: String,
    kind: &'static str,
    chapters: Vec<Chapter>,
    outline: Vec<OutlineEntry>,
    word: Option<word::Metadata>,
    saved: Value,
}

struct OpenBook {
    source: Source,
    id: String,
}

struct App {
    secret: String,
    data: PathBuf,
    recent: Mutex<Vec<Recent>>,
    books: Mutex<std::collections::HashMap<String, Arc<OpenBook>>>,
    initial: Mutex<Option<PathBuf>>,
    dropped: Mutex<std::collections::VecDeque<PathBuf>>,
    #[cfg(windows)]
    restart: Mutex<Option<platform::Restart>>,
    #[cfg(windows)]
    updater: platform::Updater,
}

type Shared = Arc<App>;
type Error = (StatusCode, String);
type Result<T> = std::result::Result<T, Error>;

fn bad(message: impl ToString) -> Error {
    (StatusCode::BAD_REQUEST, message.to_string())
}
fn internal(message: impl ToString) -> Error {
    (StatusCode::INTERNAL_SERVER_ERROR, message.to_string())
}

fn authorize(headers: &HeaderMap, app: &App) -> Result<()> {
    if headers
        .get("x-hayaku-key")
        .and_then(|value| value.to_str().ok())
        == Some(&app.secret)
    {
        Ok(())
    } else {
        Err((StatusCode::FORBIDDEN, "无效的应用会话".into()))
    }
}

fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let mut file =
        tempfile::NamedTempFile::new_in(path.parent().ok_or(io::ErrorKind::InvalidInput)?)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

fn digest(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 131_072];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn open(app: &App, path: PathBuf, relocate: Option<String>) -> Result<Document> {
    let path = dunce_path(&path).map_err(|error| bad(format!("无法打开文件：{error}")))?;
    let title = path
        .file_name()
        .map(|part| part.to_string_lossy().into_owned())
        .ok_or_else(|| bad("文件名无效"))?;
    let digest = digest(&path).map_err(bad)?;
    let source = formats::load(&path).map_err(bad)?;
    let mut recent = app.recent.lock().map_err(internal)?;
    let existing = if let Some(id) = relocate {
        let item = recent
            .iter()
            .find(|item| item.id == id)
            .ok_or_else(|| bad("找不到这份文档的阅读记录"))?;
        if item.digest != digest {
            return Err(bad("新路径的文件内容与原记录不同"));
        }
        Some(item)
    } else {
        recent.iter().find(|item| item.path == path).or_else(|| {
            recent
                .iter()
                .find(|item| item.digest == digest && !item.path.exists())
        })
    };
    let id = existing
        .map(|item| item.id.clone())
        .unwrap_or_else(|| Uuid::new_v4().to_string());
    let saved = read_json(&app.data.join(format!("{id}.json"))).map_err(internal)?;
    let (kind, chapters, outline, word) = match &source {
        Source::Pdf(_) => ("pdf", vec![], vec![], None),
        Source::Word(document) => ("word", vec![], vec![], Some(document.metadata.clone())),
        Source::Text(_) => ("text", vec![], vec![], None),
        Source::Html(_) => ("html", vec![], vec![], None),
        Source::Markdown(_) => ("markdown", vec![], vec![], None),
        Source::Epub(_, chapters, outline) => ("epub", chapters.clone(), outline.clone(), None),
        Source::Kindle(_, chapters, outline) => {
            ("kindle", chapters.clone(), outline.clone(), None)
        }
    };
    let session = Uuid::new_v4().to_string();
    let mut books = app.books.lock().map_err(internal)?;
    let mut next = recent.clone();
    next.retain(|item| item.id != id);
    next.insert(
        0,
        Recent {
            id: id.clone(),
            path,
            digest: digest.clone(),
        },
    );
    next.truncate(200);
    write_json(&app.data.join("recent.json"), &next).map_err(internal)?;
    *recent = next;
    books.insert(
        session.clone(),
        Arc::new(OpenBook {
            source,
            id: id.clone(),
        }),
    );
    Ok(Document {
        session,
        id,
        digest,
        title,
        kind,
        chapters,
        outline,
        word,
        saved,
    })
}

fn dunce_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    #[cfg(windows)]
    if let Some(path) = canonical
        .to_str()
        .and_then(|value| value.strip_prefix(r"\\?\"))
    {
        if let Some(unc) = path.strip_prefix(r"UNC\") {
            return Ok(PathBuf::from(format!(r"\\{unc}")));
        }
        if path.as_bytes().get(1) == Some(&b':') {
            return Ok(path.into());
        }
    }
    Ok(canonical)
}

async fn recent(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Vec<Recent>>> {
    authorize(&headers, &app)?;
    Ok(Json(
        app.recent
            .lock()
            .map_err(internal)?
            .iter()
            .take(30)
            .cloned()
            .collect(),
    ))
}

async fn delete_recent(
    State(app): State<Shared>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    let next = {
        let mut recent = app.recent.lock().map_err(internal)?;
        recent.retain(|item| item.id != id);
        recent.clone()
    };
    write_json(&app.data.join("recent.json"), &next).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn clear_recent(State(app): State<Shared>, headers: HeaderMap) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    {
        let mut recent = app.recent.lock().map_err(internal)?;
        recent.clear();
    }
    write_json(&app.data.join("recent.json"), &Vec::<Recent>::new()).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn settings(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    Ok(Json(
        read_json(&app.data.join("settings.json")).map_err(internal)?,
    ))
}

async fn workspace(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    Ok(Json(
        read_json(&app.data.join("workspace.json")).map_err(internal)?,
    ))
}

async fn save_workspace(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    write_json(&app.data.join("workspace.json"), &value).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn save_settings(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    write_json(&app.data.join("settings.json"), &value).map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct Import {
    name: String,
}

async fn import(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(import): Query<Import>,
    data: Bytes,
) -> Result<Json<Document>> {
    authorize(&headers, &app)?;
    tokio::task::spawn_blocking(move || {
        let name = &import.name;
        let name_path = Path::new(name);
        let extension = name_path
            .extension()
            .and_then(|part| part.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if name_path.file_name().and_then(|part| part.to_str()) != Some(name)
            || name.chars().any(|c| {
                c.is_control() || matches!(c, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
            })
            || !formats::EXTENSIONS.contains(&extension.as_str())
        {
            return Err(bad("文件名或格式无效"));
        }
        let directory = app
            .data
            .join("imports")
            .join(format!("{:x}", Sha256::digest(&data)));
        fs::create_dir_all(&directory).map_err(internal)?;
        let path = directory.join(name);
        if !path.is_file() {
            let mut temporary = tempfile::NamedTempFile::new_in(&directory).map_err(internal)?;
            temporary.write_all(&data).map_err(internal)?;
            temporary.persist(&path).map_err(internal)?;
        }
        Ok(Json(open(&app, path, None)?))
    })
    .await
    .map_err(internal)?
}

#[derive(Deserialize)]
struct OpenRequest {
    id: Option<String>,
    relocate: Option<String>,
    initial: Option<bool>,
    dropped: Option<bool>,
}

async fn open_book(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(request): Json<OpenRequest>,
) -> Result<Json<Option<Document>>> {
    authorize(&headers, &app)?;
    tokio::task::spawn_blocking(move || {
        let path = if request.initial.unwrap_or(false) {
            app.initial.lock().map_err(internal)?.take()
        } else if request.dropped.unwrap_or(false) {
            app.dropped.lock().map_err(internal)?.pop_front()
        } else if let Some(id) = request.id {
            Some(
                app.recent
                    .lock()
                    .map_err(internal)?
                    .iter()
                    .find(|item| item.id == id)
                    .ok_or_else(|| bad("找不到阅读记录"))?
                    .path
                    .clone(),
            )
        } else {
            if request.relocate.is_some() {
                rfd::FileDialog::new()
                    .add_filter("HayakuReader 文档", formats::EXTENSIONS)
                    .pick_file()
            } else {
                let chosen = rfd::FileDialog::new()
                    .add_filter("HayakuReader 文档", formats::EXTENSIONS)
                    .pick_files();
                let mut paths = chosen.into_iter().flatten();
                let first = paths.next();
                app.dropped.lock().map_err(internal)?.extend(paths);
                first
            }
        };
        path.map(|path| open(&app, path, request.relocate))
            .transpose()
            .map(Json)
    })
    .await
    .map_err(internal)?
}

fn book(app: &App, session: &str) -> Result<Arc<OpenBook>> {
    app.books
        .lock()
        .map_err(internal)?
        .get(session)
        .cloned()
        .ok_or_else(|| (StatusCode::NOT_FOUND, "文档已关闭".into()))
}

async fn save(
    State(app): State<Shared>,
    RoutePath(session): RoutePath<String>,
    headers: HeaderMap,
    Json(value): Json<Value>,
) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    let book = book(&app, &session)?;
    tokio::task::spawn_blocking(move || {
        write_json(&app.data.join(format!("{}.json", book.id)), &value)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(StatusCode::NO_CONTENT)
}

async fn close(
    State(app): State<Shared>,
    RoutePath(session): RoutePath<String>,
    headers: HeaderMap,
) -> Result<StatusCode> {
    authorize(&headers, &app)?;
    app.books.lock().map_err(internal)?.remove(&session);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
struct ContentQuery {
    chapter: Option<usize>,
    encoding: Option<String>,
    raster: Option<bool>,
    raw: Option<bool>,
}

fn parse_range(header: &str, len: u64) -> Option<std::result::Result<(u64, u64), ()>> {
    let (unit, spec) = header.trim().split_once('=')?;
    if !unit.eq_ignore_ascii_case("bytes") || spec.contains(',') {
        return None;
    }
    let (start_str, end_str) = spec.trim().split_once('-')?;
    if start_str.is_empty() && end_str.is_empty() {
        return None;
    }
    if len == 0 {
        return Some(Err(()));
    }
    if start_str.is_empty() {
        let suffix: u64 = end_str.parse().ok()?;
        if suffix == 0 {
            return Some(Err(()));
        }
        return Some(Ok((len.saturating_sub(suffix), len - 1)));
    }
    let start: u64 = start_str.parse().ok()?;
    if start >= len {
        return Some(Err(()));
    }
    if end_str.is_empty() {
        return Some(Ok((start, len - 1)));
    }
    let end: u64 = end_str.parse().ok()?;
    if end < start {
        return Some(Err(()));
    }
    Some(Ok((start, end.min(len - 1))))
}

fn pdf_response(method: Method, headers: &HeaderMap, path: &Path) -> Result<Response> {
    let mut file = fs::File::open(path).map_err(bad)?;
    let len = file.metadata().map_err(bad)?.len();
    if method == Method::HEAD {
        return Ok((
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "application/pdf".to_owned()),
                (header::ACCEPT_RANGES, "bytes".to_owned()),
                (header::CONTENT_LENGTH, len.to_string()),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                (header::CACHE_CONTROL, "no-store".to_owned()),
            ],
            Vec::<u8>::new(),
        )
            .into_response());
    }
    if let Some(range) = headers
        .get(header::RANGE)
        .and_then(|value| value.to_str().ok())
    {
        match parse_range(range, len) {
            Some(Err(())) => {
                return Ok((
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{len}"))],
                    Vec::<u8>::new(),
                )
                    .into_response());
            }
            Some(Ok((start, end))) => {
                let count = usize::try_from(end - start + 1).map_err(bad)?;
                file.seek(SeekFrom::Start(start)).map_err(bad)?;
                let mut buffer = vec![0u8; count];
                file.read_exact(&mut buffer).map_err(bad)?;
                return Ok((
                    StatusCode::PARTIAL_CONTENT,
                    [
                        (header::CONTENT_TYPE, "application/pdf".to_owned()),
                        (header::ACCEPT_RANGES, "bytes".to_owned()),
                        (header::CONTENT_RANGE, format!("bytes {start}-{end}/{len}")),
                        (header::CONTENT_LENGTH, count.to_string()),
                        (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
                        (header::CACHE_CONTROL, "no-store".to_owned()),
                    ],
                    buffer,
                )
                    .into_response());
            }
            None => {}
        }
    }
    let data = fs::read(path).map_err(bad)?;
    Ok((
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "application/pdf".to_owned()),
            (header::ACCEPT_RANGES, "bytes".to_owned()),
            (header::CONTENT_LENGTH, data.len().to_string()),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff".to_owned()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        data,
    )
        .into_response())
}

async fn content(
    method: Method,
    State(app): State<Shared>,
    RoutePath(session): RoutePath<String>,
    headers: HeaderMap,
    Query(query): Query<ContentQuery>,
) -> Result<Response> {
    authorize(&headers, &app)?;
    let book = book(&app, &session)?;
    tokio::task::spawn_blocking(move || {
        let response = match &book.source {
            Source::Pdf(path) => pdf_response(method, &headers, path)?,
            Source::Word(word) => {
                let index = query.chapter.unwrap_or(0);
                if query.raster.unwrap_or(false) {
                    bytes("image/png", word.image(index).map_err(bad)?)
                } else {
                    Json(word.text(index).map_err(bad)?).into_response()
                }
            }
            Source::Text(path) => {
                let text = formats::decode(
                    &formats::read_bytes(path).map_err(bad)?,
                    query.encoding.as_deref(),
                )
                .map_err(bad)?;
                bytes("text/plain; charset=utf-8", text.into_bytes())
            }
            Source::Html(path) => {
                let text =
                    formats::decode(&formats::read_bytes(path).map_err(bad)?, None).map_err(bad)?;
                bytes("text/plain; charset=utf-8", text.into_bytes())
            }
            Source::Markdown(path) => {
                if query.raw.unwrap_or(false) {
                    let text =
                        formats::decode(&formats::read_bytes(path).map_err(bad)?, None).map_err(bad)?;
                    bytes("text/plain; charset=utf-8", text.into_bytes())
                } else {
                    bytes(
                        "text/plain; charset=utf-8",
                        formats::markdown(path).map_err(bad)?.into_bytes(),
                    )
                }
            }
            Source::Epub(zip, chapters, _) => {
                let chapter = chapters
                    .get(query.chapter.unwrap_or(0))
                    .ok_or_else(|| bad("章节不存在"))?;
                let mut archive = zip.lock().map_err(internal)?;
                let data = formats::zip_entry(&mut archive, &chapter.path).map_err(bad)?;
                let text = formats::decode(&data, None).map_err(bad)?;
                bytes("text/plain; charset=utf-8", text.into_bytes())
            }
            Source::Kindle(book, _, _) => {
                let section = book
                    .get_section_raw(query.chapter.unwrap_or(0))
                    .ok_or_else(|| bad("章节不存在"))?;
                bytes(
                    "text/plain; charset=utf-8",
                    section.raw_html.as_bytes().to_vec(),
                )
            }
        };
        Ok(response)
    })
    .await
    .map_err(internal)?
}

#[derive(Deserialize)]
struct Resource {
    path: String,
}

async fn resource(
    State(app): State<Shared>,
    RoutePath((key, session)): RoutePath<(String, String)>,
    Query(resource): Query<Resource>,
) -> Result<Response> {
    if key != app.secret {
        return Err((StatusCode::FORBIDDEN, "无效的应用会话".into()));
    }
    let book = book(&app, &session)?;
    tokio::task::spawn_blocking(move || {
        let name = resource.path;
        let mime = match Path::new(&name)
            .extension()
            .and_then(|part| part.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "png" => "image/png",
            "jpg" | "jpeg" => "image/jpeg",
            "gif" => "image/gif",
            "webp" => "image/webp",
            "avif" => "image/avif",
            "bmp" => "image/bmp",
            "svg" => "image/svg+xml",
            "css" => "text/plain; charset=utf-8",
            _ => return Err(bad("不支持此资源类型")),
        };
        let data = match &book.source {
            Source::Epub(zip, _, _) => {
                let mut archive = zip.lock().map_err(internal)?;
                formats::zip_entry(&mut archive, &name).map_err(bad)?
            }
            Source::Kindle(book, _, _) => {
                book.get_resource_bytes(&name)
                    .or_else(|_| book.get_resource_bytes(&format!("OEBPS/{name}")))
                    .map_err(bad)?
                    .0
            }
            Source::Html(path) | Source::Markdown(path) => {
                let relative = Path::new(&name);
                if !relative
                    .components()
                    .all(|part| matches!(part, Component::Normal(_)))
                {
                    return Err(bad("无效的资源路径"));
                }
                let root = path
                    .parent()
                    .ok_or_else(|| bad("文件目录无效"))?
                    .canonicalize()
                    .map_err(bad)?;
                let file = root.join(relative).canonicalize().map_err(bad)?;
                if !file.starts_with(&root) {
                    return Err(bad("资源位于文档目录之外"));
                }
                formats::read_bytes(&file).map_err(bad)?
            }
            _ => return Err(bad("没有内嵌图片")),
        };
        // SVG is rendered as an image in the reader and sandboxed if navigated directly.
        let mut response = bytes(mime, data);
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            "sandbox; default-src 'none'; style-src 'unsafe-inline'"
                .parse()
                .unwrap(),
        );
        Ok(response)
    })
    .await
    .map_err(internal)?
}

fn bytes(mime: &'static str, data: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        data,
    )
        .into_response()
}

fn static_bytes(
    mime: &'static str,
    data: &'static [u8],
    cache_control: &'static str,
) -> Response {
    (
        [
            (header::CONTENT_TYPE, mime),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::CACHE_CONTROL, cache_control),
        ],
        Body::from(data),
    )
        .into_response()
}

async fn index() -> Response {
    let mut response = static_bytes(
        "text/html; charset=utf-8",
        include_bytes!("../ui/index.html"),
        "no-store",
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data: blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    response
}

async fn frontend(RoutePath(file): RoutePath<String>) -> Result<Response> {
    match file.as_str() {
        "reader.js" => Ok(static_bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/reader.js"),
            "private, max-age=3600",
        )),
        "pdf.js" => Ok(static_bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/pdf.js"),
            "private, max-age=3600",
        )),
        "word.js" => Ok(static_bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/word.js"),
            "private, max-age=3600",
        )),
        "theme-color.js" => Ok(static_bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/theme-color.js"),
            "private, max-age=3600",
        )),
        "code.js" => Ok(static_bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/code.js"),
            "private, max-age=3600",
        )),
        "reader.css" => Ok(static_bytes(
            "text/css; charset=utf-8",
            include_bytes!("../ui/reader.css"),
            "private, max-age=3600",
        )),
        "logo.svg" => Ok(static_bytes(
            "image/svg+xml",
            include_bytes!("../assets/logo.svg"),
            "private, max-age=3600",
        )),
        _ => Err((StatusCode::NOT_FOUND, "资源不存在".into())),
    }
}

const CMAPS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/node_modules/pdfjs-dist/cmaps");
const STANDARD_FONTS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/node_modules/pdfjs-dist/standard_fonts");
const ICCS: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/node_modules/pdfjs-dist/iccs");
const WASM: include_dir::Dir<'_> =
    include_dir::include_dir!("$CARGO_MANIFEST_DIR/node_modules/pdfjs-dist/wasm");

async fn vendor(RoutePath(file): RoutePath<String>) -> Result<Response> {
    let (data, mime): (&[u8], &'static str) = match file.as_str() {
        "purify.mjs" => (
            include_bytes!("../node_modules/dompurify/dist/purify.es.mjs"),
            "text/javascript",
        ),
        "pdf.css" => (
            include_bytes!("../node_modules/pdfjs-dist/web/pdf_viewer.css"),
            "text/css",
        ),
        "pdf.mjs" => (
            include_bytes!("../node_modules/pdfjs-dist/build/pdf.min.mjs"),
            "text/javascript",
        ),
        "pdf.worker.mjs" => (
            include_bytes!("../node_modules/pdfjs-dist/build/pdf.worker.min.mjs"),
            "text/javascript",
        ),
        _ => {
            let Some((directory, name)) = file.split_once('/') else {
                return Err(bad("资源不存在"));
            };
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
                || matches!(name, "." | "..")
            {
                return Err(bad("无效的资源路径"));
            }
            let (assets, mime) = match directory {
                "cmaps" => (&CMAPS, "application/octet-stream"),
                "standard_fonts" => (&STANDARD_FONTS, "application/octet-stream"),
                "iccs" => (&ICCS, "application/octet-stream"),
                "wasm" if name.ends_with(".wasm") => (&WASM, "application/wasm"),
                "wasm" if name.ends_with(".js") => (&WASM, "text/javascript"),
                _ => return Err(bad("资源不存在")),
            };
            let asset = assets
                .get_file(name)
                .ok_or_else(|| (StatusCode::NOT_FOUND, "资源不存在".into()))?;
            (asset.contents(), mime)
        }
    };
    Ok(static_bytes(mime, data, "public, max-age=86400, immutable"))
}

fn app_data() -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("HAYAKU_DATA") {
        return Ok(path.into());
    }
    #[cfg(windows)]
    if let Some(path) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(path).join("HayakuReader"));
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("HayakuReader"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".local/share/HayakuReader"));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "找不到本地数据目录",
    ))
}

fn main() {
    if let Err(error) = run() {
        eprintln!("HayakuReader：{error}");
        #[cfg(windows)]
        if std::env::args_os().any(|argument| argument == "--apply-update") {
            platform::report_replace_failure(error.as_ref());
        } else if !std::env::args_os().any(|argument| argument == "--no-browser") {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("HayakuReader")
                .set_description(format!("无法启动阅读器：{error}"))
                .show();
        }
        std::process::exit(1);
    }
}

fn run() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut manual = false;
    let mut launch_paths = std::collections::VecDeque::new();
    let data = app_data()?;
    fs::create_dir_all(&data)?;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        if argument == "--no-browser" {
            manual = true;
        } else if argument == "--apply-update" {
            #[cfg(windows)]
            platform::apply_update(
                &data,
                &args.next().ok_or("Missing version")?.to_string_lossy(),
                args,
            )?;
            return Ok(());
        } else {
            let path = PathBuf::from(argument);
            launch_paths.push_back(std::path::absolute(&path)?);
        }
    }
    #[cfg(windows)]
    if !manual && platform::check_manual_update(&data)? {
        return Ok(());
    }
    let instance = if manual {
        None
    } else {
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data.join("instance.lock"))?;
        match lock.try_lock() {
            Ok(()) => Some(lock),
            Err(std::fs::TryLockError::WouldBlock) => {
                let inbox = data.join("inbox");
                fs::create_dir_all(&inbox)?;
                write_json(
                    &inbox.join(format!("{}.json", Uuid::new_v4())),
                    &launch_paths,
                )?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        }
    };
    #[cfg(windows)]
    if !manual {
        platform::cleanup_updates(&data);
        platform::record_version(&data)?;
    }
    let initial = launch_paths.pop_front();
    let has_initial = initial.is_some();
    let app = Arc::new(App {
        secret: Uuid::new_v4().to_string(),
        recent: Mutex::new(read_json(&data.join("recent.json"))?),
        data,
        books: Mutex::new(std::collections::HashMap::new()),
        initial: Mutex::new(initial),
        dropped: Mutex::new(launch_paths),
        #[cfg(windows)]
        restart: Mutex::new(None),
        #[cfg(windows)]
        updater: platform::Updater::default(),
    });
    let runtime = tokio::runtime::Runtime::new()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
    let query = if manual { "dev" } else { "desktop" };
    let open = if has_initial { "&open" } else { "" };
    let url = format!(
        "http://{}/?{query}{open}#{}",
        listener.local_addr()?,
        app.secret
    );
    let router = Router::new()
        .route("/", get(index))
        .route("/ui/{file}", get(frontend))
        .route("/vendor/{*file}", get(vendor))
        .route("/api/recent", get(recent).delete(clear_recent))
        .route("/api/recent/{id}", delete(delete_recent))
        .route("/api/settings", get(settings).post(save_settings))
        .route("/api/workspace", get(workspace).post(save_workspace))
        .route("/api/open", post(open_book))
        .route(
            "/api/import",
            post(import).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/api/content/{session}", get(content))
        .route("/api/save/{session}", post(save))
        .route("/api/close/{session}", post(close))
        .route("/api/project/local", get(local_project))
        .route("/resource/{key}/{session}", get(resource));
    #[cfg(windows)]
    let router = router
        .route(
            "/api/installation",
            get(platform::status).post(platform::install),
        )
        .route(
            "/api/installation/defaults",
            post(platform::windows_defaults),
        )
        .route("/api/project", get(platform::project))
        .route("/api/project/open", post(platform::open_repo))
        .route("/api/release/progress", get(platform::progress))
        .route("/api/release/open", post(platform::open_releases))
        .route(
            "/api/release",
            get(platform::releases).post(platform::update),
        );
    let router = router.with_state(app.clone());
    if manual {
        println!("HayakuReader · {url}");
        runtime.block_on(async { axum::serve(listener, router).await })?;
        return Ok(());
    }
    #[cfg(windows)]
    {
        runtime.spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                eprintln!("HayakuReader：{error}");
            }
        });
        desktop::run(&url, app.clone())?;
        let pending = app
            .restart
            .lock()
            .map_err(|_| "Restart state unavailable")?
            .take();
        drop(instance);
        if let Some(request) = pending {
            platform::restart(request)?;
        }
    }
    #[cfg(not(windows))]
    return Err("当前桌面窗口适用于 Windows，可用 --no-browser 启动开发服务".into());
    #[cfg(windows)]
    Ok(())
}

#[derive(Deserialize)]
struct Language {
    lang: Option<String>,
}

async fn local_project(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(language): Query<Language>,
) -> Result<String> {
    authorize(&headers, &app)?;
    Ok(formats::render_markdown(
        if language.lang.as_deref() == Some("en") {
            include_str!("../docs/intro.en.md")
        } else {
            include_str!("../docs/intro.zh.md")
        },
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(data: PathBuf) -> App {
        App {
            secret: "test-secret".into(),
            data,
            recent: Mutex::new(Vec::new()),
            books: Mutex::new(std::collections::HashMap::new()),
            initial: Mutex::new(None),
            dropped: Mutex::new(std::collections::VecDeque::new()),
            #[cfg(windows)]
            restart: Mutex::new(None),
            #[cfg(windows)]
            updater: platform::Updater::default(),
        }
    }

    #[test]
    fn authorize_checks_session_header() {
        let dir = tempfile::tempdir().unwrap();
        let app = test_app(dir.path().to_path_buf());
        let mut headers = HeaderMap::new();
        assert!(authorize(&headers, &app).is_err());
        headers.insert("x-hayaku-key", "wrong".parse().unwrap());
        assert!(authorize(&headers, &app).is_err());
        headers.insert("x-hayaku-key", "test-secret".parse().unwrap());
        assert!(authorize(&headers, &app).is_ok());
    }

    #[test]
    fn parse_range_handles_valid_and_invalid_ranges() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(Ok((0, 9))));
        assert_eq!(parse_range("bytes=50-999", 100), Some(Ok((50, 99))));
        assert_eq!(parse_range("bytes=80-", 100), Some(Ok((80, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=-200", 100), Some(Ok((0, 99))));
        assert_eq!(parse_range("bytes=100-105", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=10-5", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=-0", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=0-0", 0), Some(Err(())));
        assert_eq!(parse_range("bytes=0-1, 2-3", 100), None);
        assert_eq!(parse_range("items=0-5", 100), None);
    }

    #[test]
    fn read_and_write_json_roundtrip_and_default() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("state.json");
        let missing: Vec<String> = read_json(&path)?;
        assert!(missing.is_empty());
        let sample = vec!["alpha".to_owned(), "beta".to_owned()];
        write_json(&path, &sample)?;
        let loaded: Vec<String> = read_json(&path)?;
        assert_eq!(loaded, sample);
        Ok(())
    }

    #[test]
    fn dunce_path_strips_windows_verbatim_prefix() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("book.txt");
        fs::write(&file, b"hello")?;
        let normalized = dunce_path(&file)?;
        assert!(!normalized.to_string_lossy().starts_with(r"\\?\"));
        assert!(normalized.is_file());
        Ok(())
    }

    #[tokio::test]
    async fn delete_and_clear_recent_updates_memory_and_disk() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let app = Arc::new(test_app(dir.path().to_path_buf()));
        {
            let mut recent = app.recent.lock().unwrap();
            recent.push(Recent {
                id: "id-1".into(),
                path: PathBuf::from("a.pdf"),
                digest: "d1".into(),
            });
            recent.push(Recent {
                id: "id-2".into(),
                path: PathBuf::from("b.epub"),
                digest: "d2".into(),
            });
        }
        let mut headers = HeaderMap::new();
        headers.insert("x-hayaku-key", "test-secret".parse().unwrap());
        assert_eq!(
            delete_recent(
                State(app.clone()),
                RoutePath("id-1".into()),
                headers.clone()
            )
            .await
            .unwrap(),
            StatusCode::NO_CONTENT
        );
        let stored: Vec<Recent> = read_json(&dir.path().join("recent.json"))?;
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].id, "id-2");

        assert_eq!(
            clear_recent(State(app.clone()), headers).await.unwrap(),
            StatusCode::NO_CONTENT
        );
        assert!(app.recent.lock().unwrap().is_empty());
        let cleared: Vec<Recent> = read_json(&dir.path().join("recent.json"))?;
        assert!(cleared.is_empty());
        Ok(())
    }
}
