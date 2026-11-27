use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

pub const EXTENSIONS: &[&str] = &[
    "pdf", "epub", "txt", "azw3", "azw", "mobi", "doc", "docx", "md", "markdown", "html", "htm",
    "xhtml", "xml",
];

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recent {
    pub id: String,
    pub path: PathBuf,
    pub digest: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    #[serde(rename = "font-size")]
    pub font_size: f32,
    #[serde(rename = "line-height")]
    pub line_height: f32,
    #[serde(rename = "text-width")]
    pub text_width: f32,
    #[serde(rename = "restore-session")]
    pub restore_session: bool,
    #[serde(rename = "auto-update")]
    pub auto_update: bool,
    #[serde(rename = "install-dismissed")]
    pub install_dismissed: bool,
    #[serde(rename = "smart-invert")]
    pub smart_invert: bool,
    #[serde(rename = "smart-invert-hint-seen")]
    pub smart_invert_hint_seen: bool,
    #[serde(rename = "tab-sleep-hint-seen")]
    pub tab_sleep_hint_seen: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".to_owned(),
            font_size: 19.0,
            line_height: 1.8,
            text_width: 760.0,
            restore_session: true,
            auto_update: false,
            install_dismissed: false,
            smart_invert: true,
            smart_invert_hint_seen: false,
            tab_sleep_hint_seen: false,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TabEntry {
    pub id: String,
    pub title: String,
    #[serde(rename = "lastUsed", default)]
    pub last_used: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Workspace {
    #[serde(default)]
    pub tabs: Vec<TabEntry>,
    pub active: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Position {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub fraction: f32,
    #[serde(default)]
    pub anchor: Option<Anchor>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Anchor {
    #[serde(default)]
    pub index: usize,
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub quote: String,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub before: String,
    #[serde(default)]
    pub after: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Annotation {
    pub id: String,
    pub kind: String,
    #[serde(default)]
    pub color: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub parts: Vec<Anchor>,
    #[serde(default)]
    pub position: Option<Position>,
    #[serde(rename = "viewMode", default)]
    pub view_mode: Option<String>,
    #[serde(default)]
    pub digest: String,
    #[serde(default)]
    pub missing: bool,
    #[serde(rename = "createdAt", default)]
    pub created_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct BookState {
    pub position: Option<Position>,
    pub digest: String,
    pub encoding: Option<String>,
    #[serde(rename = "viewMode")]
    pub view_mode: Option<String>,
    pub annotations: Vec<Annotation>,
}

pub fn app_data() -> io::Result<PathBuf> {
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

pub fn read_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> io::Result<T> {
    match fs::read(path) {
        Ok(bytes) => Ok(serde_json::from_slice(&bytes).unwrap_or_default()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(T::default()),
        Err(error) => Err(error),
    }
}

pub fn write_json(path: &Path, value: &impl Serialize) -> io::Result<()> {
    let parent = path.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer(&mut file, value)?;
    file.flush()?;
    file.as_file().sync_all()?;
    file.persist(path)?;
    Ok(())
}

pub fn file_digest(path: &Path) -> io::Result<String> {
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

pub fn dunce_path(path: &Path) -> io::Result<PathBuf> {
    let canonical = match path.canonicalize() {
        Ok(resolved) => resolved,
        Err(error) => {
            let abs = std::path::absolute(path)?;
            if fs::metadata(&abs).is_ok() {
                return Ok(abs);
            }
            return Err(error);
        }
    };
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

pub fn delete_recent_entry(data: &Path, recent: &mut Vec<Recent>, id: &str) -> io::Result<()> {
    recent.retain(|item| item.id != id);
    write_json(&data.join("recent.json"), recent)
}

pub fn clear_recent_entries(data: &Path, recent: &mut Vec<Recent>) -> io::Result<()> {
    recent.clear();
    write_json(&data.join("recent.json"), recent)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn delete_and_clear_recent_updates_memory_and_disk() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let mut recent = vec![
            Recent {
                id: "id-1".into(),
                path: PathBuf::from("a.pdf"),
                digest: "d1".into(),
            },
            Recent {
                id: "id-2".into(),
                path: PathBuf::from("b.epub"),
                digest: "d2".into(),
            },
        ];
        delete_recent_entry(dir.path(), &mut recent, "id-1")?;
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].id, "id-2");
        let stored: Vec<Recent> = read_json(&dir.path().join("recent.json"))?;
        assert_eq!(stored, recent);

        clear_recent_entries(dir.path(), &mut recent)?;
        assert!(recent.is_empty());
        let cleared: Vec<Recent> = read_json(&dir.path().join("recent.json"))?;
        assert!(cleared.is_empty());
        Ok(())
    }

    #[test]
    fn annotation_view_mode_serializes_and_defaults_none_for_legacy_json() {
        let code_ann = Annotation {
            id: "a1".into(),
            kind: "highlight".into(),
            color: "yellow".into(),
            text: "snippet".into(),
            view_mode: Some("code".into()),
            ..Default::default()
        };
        let encoded = serde_json::to_string(&code_ann).expect("serialize annotation");
        assert!(encoded.contains(r#""viewMode":"code""#));
        let decoded: Annotation = serde_json::from_str(&encoded).expect("deserialize annotation");
        assert_eq!(decoded.view_mode.as_deref(), Some("code"));

        let legacy_json = r#"{"id":"a0","kind":"note","text":"old"}"#;
        let legacy: Annotation = serde_json::from_str(legacy_json).expect("deserialize legacy");
        assert_eq!(legacy.view_mode, None);
    }
}
