#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

#[cfg(windows)]
mod desktop;
mod formats;
mod word;

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, Path as RoutePath, Query, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use formats::{Chapter, Source};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
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
    dropped: Mutex<Option<PathBuf>>,
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
    let mut buffer = [0; 32768];
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
    let (kind, chapters, word) = match &source {
        Source::Pdf(_) => ("pdf", vec![], None),
        Source::Word(document) => ("word", vec![], Some(document.metadata.clone())),
        Source::Text(_) => ("text", vec![], None),
        Source::Html(_) => ("html", vec![], None),
        Source::Markdown(_) => ("markdown", vec![], None),
        Source::Epub(_, chapters) => ("epub", chapters.clone(), None),
        Source::Kindle(_, chapters) => ("kindle", chapters.clone(), None),
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

async fn settings(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    Ok(Json(
        read_json(&app.data.join("settings.json")).map_err(internal)?,
    ))
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
            app.dropped.lock().map_err(internal)?.take()
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
            rfd::FileDialog::new()
                .add_filter("HayakuReader 文档", formats::EXTENSIONS)
                .pick_file()
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
}

async fn content(
    State(app): State<Shared>,
    RoutePath(session): RoutePath<String>,
    headers: HeaderMap,
    Query(query): Query<ContentQuery>,
) -> Result<Response> {
    authorize(&headers, &app)?;
    let book = book(&app, &session)?;
    tokio::task::spawn_blocking(move || {
        let response = match &book.source {
            Source::Pdf(path) => bytes("application/pdf", formats::read_bytes(path).map_err(bad)?),
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
            Source::Markdown(path) => bytes(
                "text/plain; charset=utf-8",
                formats::markdown(path).map_err(bad)?.into_bytes(),
            ),
            Source::Epub(path, chapters) => {
                let chapter = chapters
                    .get(query.chapter.unwrap_or(0))
                    .ok_or_else(|| bad("章节不存在"))?;
                let data = formats::archive_entry(path, &chapter.path).map_err(bad)?;
                let text = formats::decode(&data, None).map_err(bad)?;
                bytes("text/plain; charset=utf-8", text.into_bytes())
            }
            Source::Kindle(book, _) => {
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
            Source::Epub(path, _) => formats::archive_entry(path, &name).map_err(bad)?,
            Source::Kindle(book, _) => {
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

async fn index() -> Response {
    let mut response = bytes(
        "text/html; charset=utf-8",
        include_bytes!("../ui/index.html").to_vec(),
    );
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY,
        "default-src 'none'; script-src 'self' 'wasm-unsafe-eval'; worker-src 'self' blob:; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data: blob:; connect-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'".parse().unwrap());
    response
}

async fn frontend(RoutePath(file): RoutePath<String>) -> Result<Response> {
    match file.as_str() {
        "reader.js" => Ok(bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/reader.js").to_vec(),
        )),
        "pdf.js" => Ok(bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/pdf.js").to_vec(),
        )),
        "word.js" => Ok(bytes(
            "text/javascript; charset=utf-8",
            include_bytes!("../ui/word.js").to_vec(),
        )),
        "reader.css" => Ok(bytes(
            "text/css; charset=utf-8",
            include_bytes!("../ui/reader.css").to_vec(),
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
    Ok(bytes(mime, data.to_vec()))
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
        if !std::env::args_os().any(|argument| argument == "--no-browser") {
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
    let mut initial = None;
    for argument in std::env::args_os().skip(1) {
        if argument == "--no-browser" {
            manual = true;
        } else {
            initial = Some(PathBuf::from(argument));
        }
    }
    let data = app_data()?;
    fs::create_dir_all(&data)?;
    let has_initial = initial.is_some();
    let app = Arc::new(App {
        secret: Uuid::new_v4().to_string(),
        recent: Mutex::new(read_json(&data.join("recent.json"))?),
        data,
        books: Mutex::new(std::collections::HashMap::new()),
        initial: Mutex::new(initial),
        dropped: Mutex::new(None),
    });
    let runtime = tokio::runtime::Runtime::new()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))?;
    let query = if has_initial { "?open" } else { "" };
    let url = format!("http://{}/{query}#{}", listener.local_addr()?, app.secret);
    let router = Router::new()
        .route("/", get(index))
        .route("/ui/{file}", get(frontend))
        .route("/vendor/{*file}", get(vendor))
        .route("/api/recent", get(recent))
        .route("/api/settings", get(settings).post(save_settings))
        .route("/api/open", post(open_book))
        .route(
            "/api/import",
            post(import).layer(DefaultBodyLimit::max(64 * 1024 * 1024)),
        )
        .route("/api/content/{session}", get(content))
        .route("/api/save/{session}", post(save))
        .route("/api/close/{session}", post(close))
        .route("/resource/{key}/{session}", get(resource))
        .with_state(app.clone());
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
        desktop::run(&url, app)?;
    }
    #[cfg(not(windows))]
    return Err("当前桌面窗口适用于 Windows，可用 --no-browser 启动开发服务".into());
    #[cfg(windows)]
    Ok(())
}
