use crate::{App, Result, Shared, authorize, bad, internal, read_json, write_json};
use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};
use uuid::Uuid;
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

const REPO: &str = "https://github.com/drliuhuan/HayakuReader";
const RELEASE_API: &str = "https://api.github.com/repos/drliuhuan/HayakuReader/releases/latest";
const PROG_ID: &str = "HayakuReader.Document";
const CAPABILITIES: &str = r"Software\HayakuReader\Capabilities";
const VERSION: &str = env!("CARGO_PKG_VERSION");

pub enum Restart {
    Installed,
    Update {
        executable: PathBuf,
        version: String,
    },
}

pub fn installed_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or(io::ErrorKind::NotFound)?;
    Ok(PathBuf::from(local).join("Programs/HayakuReader/HayakuReader.exe"))
}

fn installed() -> bool {
    installed_path().is_ok_and(|target| {
        target
            .canonicalize()
            .ok()
            .zip(
                std::env::current_exe()
                    .ok()
                    .and_then(|path| path.canonicalize().ok()),
            )
            .is_some_and(|(target, current)| target == current)
    })
}

fn copy_binary(source: &Path, target: &Path) -> io::Result<()> {
    let parent = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    io::copy(&mut fs::File::open(source)?, &mut temp)?;
    temp.as_file().sync_all()?;
    temp.persist(target)?;
    Ok(())
}

fn version_tuple(version: &str) -> Option<[u64; 3]> {
    let mut parts = version.trim_start_matches('v').split('.');
    let result = [
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
        parts.next()?.parse().ok()?,
    ];
    parts.next().is_none().then_some(result)
}

pub fn record_version(app_data: &Path) -> io::Result<()> {
    if installed() {
        write_json(&app_data.join("installed-version.json"), &VERSION)?;
    }
    Ok(())
}

pub fn cleanup_updates(data: &Path) {
    let Ok(entries) = fs::read_dir(data) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("update-") && name.ends_with(".exe")
            })
        {
            let _ = fs::remove_file(path);
        }
    }
}

pub fn check_manual_update(data: &Path) -> io::Result<bool> {
    if installed() {
        return Ok(false);
    }
    let target = installed_path()?;
    if !target.is_file() {
        return Ok(false);
    }
    let previous: String = read_json(&data.join("installed-version.json"))?;
    if !version_tuple(&previous)
        .zip(version_tuple(VERSION))
        .is_some_and(|(previous, current)| current > previous)
    {
        Command::new(target)
            .args(std::env::args_os().skip(1))
            .spawn()?;
        return Ok(true);
    }
    let accepted = rfd::MessageDialog::new()
        .set_title("HayakuReader")
        .set_description(format!(
            "Update the installed HayakuReader from {previous} to {VERSION}?"
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show()
        == rfd::MessageDialogResult::Yes;
    if accepted {
        let staging = data.join(format!("update-{}.exe", Uuid::new_v4()));
        copy_binary(&std::env::current_exe()?, &staging)?;
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data.join("instance.lock"))?;
        if matches!(lock.try_lock(), Err(std::fs::TryLockError::WouldBlock)) {
            fs::write(data.join("request-restart"), [])?;
        }
        drop(lock);
        Command::new(staging)
            .args(["--apply-update", VERSION])
            .args(std::env::args_os().skip(1))
            .spawn()?;
    }
    Ok(true)
}

fn association_defaults() -> Vec<String> {
    crate::formats::EXTENSIONS
        .iter()
        .filter(|extension| !matches!(**extension, "txt" | "doc" | "docx" | "htm" | "html"))
        .map(|extension| (*extension).to_owned())
        .collect()
}

fn associations(app: &App) -> io::Result<Vec<String>> {
    let path = app.data.join("associations.json");
    if path.is_file() {
        read_json(&path)
    } else {
        Ok(association_defaults())
    }
}

fn remove_value(key: &RegKey, name: &str) -> io::Result<()> {
    match key.delete_value(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn register(app: &App, selected: &[String]) -> io::Result<()> {
    let exe = installed_path()?;
    if !exe.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "HayakuReader is not installed",
        ));
    }
    let root = RegKey::predef(HKEY_CURRENT_USER);
    register_associations(&root, &app.data, &exe, selected)?;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            first: *const core::ffi::c_void,
            second: *const core::ffi::c_void,
        );
    }
    // Tell the Windows shell to refresh file association registrations.
    unsafe {
        SHChangeNotify(0x08000000, 0, std::ptr::null(), std::ptr::null());
    }
    Ok(())
}

fn run_shortcut_script(target: &Path, explicit_links: Option<&[&Path]>) -> io::Result<()> {
    let workdir = target.parent().ok_or(io::ErrorKind::InvalidInput)?;
    let script = r#"
$ErrorActionPreference = 'Stop'
$shell = New-Object -ComObject WScript.Shell
$paths = if ($env:HAYAKU_LINKS) {
    $env:HAYAKU_LINKS -split "`n" | Where-Object { $_ }
} else {
    $folders = @([Environment]::GetFolderPath('Desktop'), [Environment]::GetFolderPath('Programs'))
    foreach ($folder in $folders) {
        if ($folder) { Join-Path $folder 'HayakuReader.lnk' }
    }
}
foreach ($linkPath in $paths) {
    $parent = Split-Path -Parent $linkPath
    if ($parent) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }
    $shortcut = $shell.CreateShortcut($linkPath)
    $shortcut.TargetPath = $env:HAYAKU_EXE
    $shortcut.WorkingDirectory = $env:HAYAKU_WORKDIR
    $shortcut.IconLocation = "$env:HAYAKU_EXE,0"
    $shortcut.Description = 'HayakuReader'
    $shortcut.Save()
}
"#;
    let mut command = Command::new("powershell.exe");
    command
        .creation_flags(0x08000000)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", script])
        .env("HAYAKU_EXE", target)
        .env("HAYAKU_WORKDIR", workdir);
    if let Some(links) = explicit_links {
        let joined = links
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("\n");
        command.env("HAYAKU_LINKS", joined);
    } else {
        command.env_remove("HAYAKU_LINKS");
    }
    let status = command.status()?;
    if !status.success() {
        return Err(io::Error::other("Could not create Windows shortcuts"));
    }
    Ok(())
}

pub fn create_shortcuts(target: &Path) -> io::Result<()> {
    run_shortcut_script(target, None)
}

fn register_associations(
    root: &RegKey,
    data: &Path,
    exe: &Path,
    selected: &[String],
) -> io::Result<()> {
    let (prog, _) = root.create_subkey(format!(r"Software\Classes\{PROG_ID}"))?;
    prog.set_value("", &"HayakuReader document")?;
    prog.create_subkey(r"shell\open\command")?
        .0
        .set_value("", &format!("\"{}\" \"%1\"", exe.display()))?;
    prog.create_subkey("DefaultIcon")?
        .0
        .set_value("", &format!("\"{}\",0", exe.display()))?;
    let (registered, _) = root.create_subkey(r"Software\RegisteredApplications")?;
    registered.set_value("HayakuReader", &CAPABILITIES)?;
    let (capabilities, _) = root.create_subkey(CAPABILITIES)?;
    capabilities.set_value("ApplicationName", &"HayakuReader")?;
    capabilities.set_value("ApplicationDescription", &"Local document reader")?;
    let (types, _) = capabilities.create_subkey("FileAssociations")?;
    let previous_file = data.join("previous-associations.json");
    let mut previous: std::collections::BTreeMap<String, Option<String>> =
        read_json(&previous_file)?;
    for extension in crate::formats::EXTENSIONS {
        let dotted = format!(".{extension}");
        let (class, _) = root.create_subkey(format!(r"Software\Classes\{dotted}"))?;
        let (handlers, _) = class.create_subkey("OpenWithProgids")?;
        let current = class.get_value::<String, _>("").ok();
        if selected.iter().any(|value| value == extension) {
            types.set_value(&dotted, &PROG_ID)?;
            handlers.set_value(PROG_ID, &"")?;
            if current.as_deref() != Some(PROG_ID) {
                previous.insert(dotted.clone(), current);
                class.set_value("", &PROG_ID)?;
            }
        } else {
            remove_value(&types, &dotted)?;
            remove_value(&handlers, PROG_ID)?;
            if current.as_deref() == Some(PROG_ID) {
                if let Some(Some(old)) = previous.remove(&dotted) {
                    class.set_value("", &old)?;
                } else {
                    remove_value(&class, "")?;
                }
            }
        }
    }
    write_json(&previous_file, &previous)?;
    write_json(&data.join("associations.json"), &selected)?;
    Ok(())
}

pub async fn windows_defaults(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<axum::http::StatusCode> {
    authorize(&headers, &app)?;
    if !installed() {
        return Err(bad("Install HayakuReader first."));
    }
    shell_open("ms-settings:defaultapps?registeredAppUser=HayakuReader")
}

pub async fn open_releases(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<axum::http::StatusCode> {
    authorize(&headers, &app)?;
    shell_open(&format!("{REPO}/releases/latest"))
}

pub async fn open_repo(
    State(app): State<Shared>,
    headers: HeaderMap,
) -> Result<axum::http::StatusCode> {
    authorize(&headers, &app)?;
    shell_open(REPO)
}

pub fn report_replace_failure(error: &dyn std::error::Error) {
    let answer = rfd::MessageDialog::new()
        .set_title("HayakuReader")
        .set_level(rfd::MessageLevel::Error)
        .set_description(format!("更新失败 / Update failed\n{error}\n\n打开 GitHub Release 手动下载？ / Open GitHub Releases to download manually?\n{REPO}/releases/latest"))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    if answer == rfd::MessageDialogResult::Yes {
        let _ = shell_open(&format!("{REPO}/releases/latest"));
    }
}

fn shell_open(address: &str) -> Result<axum::http::StatusCode> {
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn ShellExecuteW(
            window: *const core::ffi::c_void,
            operation: *const u16,
            file: *const u16,
            parameters: *const u16,
            directory: *const u16,
            show: i32,
        ) -> isize;
    }
    let uri: Vec<u16> = address.encode_utf16().chain(std::iter::once(0)).collect();
    // Callers provide fixed system and GitHub URLs.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null(),
            std::ptr::null(),
            uri.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1,
        )
    };
    if result <= 32 {
        return Err(internal("Could not open the link"));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}

pub async fn status(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    let target = installed_path().map_err(internal)?;
    Ok(Json(json!({
        "installed": installed(), "installationExists": target.is_file(), "path": target,
        "version": VERSION, "associations": associations(&app).map_err(internal)?,
        "extensions": crate::formats::EXTENSIONS,
    })))
}

#[derive(Deserialize)]
pub struct InstallRequest {
    extensions: Vec<String>,
    install: bool,
}

pub async fn install(
    State(app): State<Shared>,
    headers: HeaderMap,
    Json(config): Json<InstallRequest>,
) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    if config
        .extensions
        .iter()
        .any(|extension| !crate::formats::EXTENSIONS.contains(&extension.as_str()))
    {
        return Err(bad("Invalid file extension"));
    }
    tokio::task::spawn_blocking(move || {
        if config.install && !installed() {
            let target = installed_path().map_err(internal)?;
            if target.exists() {
                return Err(bad("HayakuReader is already installed."));
            }
            copy_binary(&std::env::current_exe().map_err(internal)?, &target).map_err(internal)?;
            write_json(&app.data.join("installed-version.json"), &VERSION).map_err(internal)?;
            register(&app, &config.extensions).map_err(internal)?;
            create_shortcuts(&target).map_err(internal)?;
            *app.restart.lock().map_err(internal)? = Some(Restart::Installed);
            return Ok(Json(json!({"restart": true})));
        }
        if !installed() {
            return Err(bad(
                "Install HayakuReader before setting file associations.",
            ));
        }
        register(&app, &config.extensions).map_err(internal)?;
        Ok(Json(json!({"restart": false})))
    })
    .await
    .map_err(internal)?
}

#[derive(Deserialize)]
pub struct Language {
    lang: Option<String>,
}

fn http(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent("HayakuReader")
        .build()
        .into()
}

pub async fn project(
    State(app): State<Shared>,
    headers: HeaderMap,
    Query(language): Query<Language>,
) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    let english = language.lang.as_deref() == Some("en");
    let local = if english {
        include_str!("../docs/intro.en.md")
    } else {
        include_str!("../docs/intro.zh.md")
    };
    let name = if english {
        "intro.en.md"
    } else {
        "intro.zh.md"
    };
    let response = tokio::task::spawn_blocking(move || {
        let url =
            format!("https://raw.githubusercontent.com/drliuhuan/HayakuReader/main/docs/{name}");
        let mut response = http(Duration::from_millis(2300)).get(url).call().ok()?;
        response
            .body_mut()
            .with_config()
            .limit(128 * 1024)
            .read_to_string()
            .ok()
    })
    .await
    .ok()
    .flatten()
    .filter(|text| !text.trim().is_empty());
    Ok(Json(
        json!({"html": crate::formats::render_markdown(response.as_deref().unwrap_or(local))}),
    ))
}

#[derive(Clone, Serialize, Deserialize)]
struct Release {
    tag_name: String,
    assets: Vec<Asset>,
}
#[derive(Clone, Serialize, Deserialize)]
struct Asset {
    name: String,
    browser_download_url: String,
    digest: Option<String>,
    size: u64,
}

#[derive(Default)]
pub struct Updater {
    release: std::sync::Mutex<Option<Release>>,
    download: std::sync::Mutex<Option<Download>>,
}

#[derive(Clone, Serialize)]
struct Download {
    downloaded: u64,
    total: u64,
    bytes_per_second: u64,
}

pub async fn progress(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    Ok(Json(json!(*app.updater.download.lock().map_err(internal)?)))
}

fn latest_once() -> std::result::Result<Option<Release>, String> {
    let mut response = match http(Duration::from_secs(8)).get(RELEASE_API).call() {
        Ok(response) => response,
        Err(ureq::Error::StatusCode(404)) => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let release: Release = response
        .body_mut()
        .read_json()
        .map_err(|error| error.to_string())?;
    Ok(version_tuple(&release.tag_name)
        .zip(version_tuple(VERSION))
        .is_some_and(|(latest, current)| latest > current)
        .then_some(release))
}

fn latest() -> std::result::Result<Option<Release>, String> {
    latest_once().or_else(|_| latest_once())
}

pub async fn releases(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    let release = tokio::task::spawn_blocking(latest)
        .await
        .map_err(internal)?
        .map_err(bad)?;
    *app.updater.release.lock().map_err(internal)? = release.clone();
    Ok(Json(
        json!({"version": release.map(|release| release.tag_name), "installed": installed()}),
    ))
}

pub async fn update(State(app): State<Shared>, headers: HeaderMap) -> Result<Json<Value>> {
    authorize(&headers, &app)?;
    if !installed() {
        return Err(bad("Install HayakuReader before applying an update."));
    }
    tokio::task::spawn_blocking(move || {
        let cached = app.updater.release.lock().map_err(internal)?.clone();
        let release = match cached {
            Some(release) => release,
            None => latest()
                .map_err(bad)?
                .ok_or_else(|| bad("No newer release"))?,
        };
        let asset = release
            .assets
            .iter()
            .find(|asset| asset.name == "HayakuReader.exe")
            .ok_or_else(|| bad("The release has no Windows executable"))?;
        let expected = format!(
            "{REPO}/releases/download/{}/HayakuReader.exe",
            release.tag_name
        );
        if asset.browser_download_url != expected {
            return Err(bad("Unexpected release asset URL"));
        }
        let hash = asset
            .digest
            .as_deref()
            .and_then(|digest| digest.strip_prefix("sha256:"))
            .ok_or_else(|| bad("The release has no SHA-256 digest"))?;
        let mut progress = app.updater.download.lock().map_err(internal)?;
        if progress.is_some() {
            return Err(bad("An update is already downloading"));
        }
        *progress = Some(Download {
            downloaded: 0,
            total: asset.size,
            bytes_per_second: 0,
        });
        drop(progress);
        let result = (|| {
            let mut response = http(Duration::from_secs(180))
                .get(expected)
                .call()
                .map_err(bad)?;
            let mut reader = response
                .body_mut()
                .with_config()
                .limit(128 * 1024 * 1024)
                .reader();
            let mut file = tempfile::NamedTempFile::new_in(&app.data).map_err(internal)?;
            let mut checksum = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            let mut downloaded = 0u64;
            let mut sampled_bytes = 0u64;
            let mut sampled_at = std::time::Instant::now();
            let mut signature = Vec::<u8>::with_capacity(2);
            loop {
                let count = reader.read(&mut buffer).map_err(bad)?;
                if count == 0 {
                    break;
                }
                if signature.len() < 2 {
                    signature.extend_from_slice(&buffer[..count.min(2 - signature.len())]);
                }
                checksum.update(&buffer[..count]);
                file.write_all(&buffer[..count]).map_err(internal)?;
                downloaded += count as u64;
                let elapsed = sampled_at.elapsed();
                let mut progress = app.updater.download.lock().map_err(internal)?;
                if let Some(progress) = progress.as_mut() {
                    progress.downloaded = downloaded;
                    if elapsed >= Duration::from_millis(400) {
                        progress.bytes_per_second =
                            ((downloaded - sampled_bytes) as f64 / elapsed.as_secs_f64()) as u64;
                        sampled_bytes = downloaded;
                        sampled_at = std::time::Instant::now();
                    }
                }
            }
            if signature != b"MZ"
                || downloaded != asset.size
                || format!("{:x}", checksum.finalize()) != hash
            {
                return Err(bad("The update file failed verification"));
            }
            let exe = app.data.join(format!("update-{}.exe", Uuid::new_v4()));
            file.as_file().sync_all().map_err(internal)?;
            file.persist(&exe).map_err(internal)?;
            *app.restart.lock().map_err(internal)? = Some(Restart::Update {
                executable: exe,
                version: release.tag_name,
            });
            Ok(Json(json!({"restart": true})))
        })();
        *app.updater.download.lock().map_err(internal)? = None;
        result
    })
    .await
    .map_err(internal)?
}

pub fn restart(request: Restart) -> io::Result<()> {
    match request {
        Restart::Installed => {
            Command::new(installed_path()?).spawn()?;
        }
        Restart::Update {
            executable,
            version,
        } => {
            Command::new(executable)
                .args(["--apply-update", &version])
                .spawn()?;
        }
    }
    Ok(())
}

pub fn apply_update(
    data: &Path,
    version: &str,
    paths: impl IntoIterator<Item = std::ffi::OsString>,
) -> io::Result<()> {
    let source = std::env::current_exe()?;
    if source
        .parent()
        .and_then(|parent| parent.canonicalize().ok())
        != Some(data.canonicalize()?)
        || !source
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with("update-"))
        || version_tuple(version) != version_tuple(VERSION)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invalid HayakuReader update",
        ));
    }
    let target = installed_path()?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.join("instance.lock"))?;
    'replace: loop {
        for _ in 0..20 {
            match lock.try_lock() {
                Ok(()) => match copy_binary(&source, &target) {
                    Ok(()) => break 'replace,
                    Err(error) if matches!(error.raw_os_error(), Some(5 | 32 | 33)) => {
                        lock.unlock()?
                    }
                    Err(error) => return Err(error),
                },
                Err(std::fs::TryLockError::WouldBlock) => {}
                Err(error) => return Err(error.into()),
            }
            std::thread::sleep(Duration::from_millis(200));
        }
        let answer = rfd::MessageDialog::new()
            .set_title("HayakuReader")
            .set_level(rfd::MessageLevel::Warning)
            .set_description("更新尚未能替换旧版本。请关闭旧版 HayakuReader。如果它仍在运行，可在任务管理器中结束旧进程，再点「确定」重试。\n\nClose the previous HayakuReader instance, or end it in Task Manager if it is stuck, then choose OK to retry.")
            .set_buttons(rfd::MessageButtons::OkCancel)
            .show();
        if answer != rfd::MessageDialogResult::Ok {
            return Ok(());
        }
    }
    drop(lock);
    Command::new(target).args(paths).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_selected_formats_and_restores_previous_default() -> io::Result<()> {
        let path = format!(r"Software\HayakuReader-test-{}", Uuid::new_v4());
        let profile = RegKey::predef(HKEY_CURRENT_USER);
        let data = tempfile::tempdir()?;
        let exe = data.path().join("HayakuReader.exe");
        let result = (|| {
            let (test_root, _) = profile.create_subkey(&path)?;
            test_root
                .create_subkey(r"Software\Classes\.pdf")?
                .0
                .set_value("", &"OtherPDF")?;
            let defaults = association_defaults();
            register_associations(&test_root, data.path(), &exe, &defaults)?;
            let selected = test_root.open_subkey(format!(r"{CAPABILITIES}\FileAssociations"))?;
            assert_eq!(selected.get_value::<String, _>(".pdf")?, PROG_ID);
            for ext in [".txt", ".doc", ".docx", ".htm", ".html"] {
                assert!(selected.get_value::<String, _>(ext).is_err());
            }
            let command =
                test_root.open_subkey(format!(r"Software\Classes\{PROG_ID}\shell\open\command"))?;
            assert_eq!(
                command.get_value::<String, _>("")?,
                format!("\"{}\" \"%1\"", exe.display())
            );
            register_associations(&test_root, data.path(), &exe, &["txt".to_owned()])?;
            assert_eq!(
                test_root
                    .open_subkey(r"Software\Classes\.pdf")?
                    .get_value::<String, _>("")?,
                "OtherPDF"
            );
            assert_eq!(selected.get_value::<String, _>(".txt")?, PROG_ID);
            Ok::<(), io::Error>(())
        })();
        let cleanup = profile.delete_subkey_all(&path);
        result.and(cleanup)
    }

    #[test]
    fn parses_and_orders_semver_versions() {
        assert_eq!(version_tuple("0.2.3"), Some([0, 2, 3]));
        assert_eq!(version_tuple("v1.10.2"), Some([1, 10, 2]));
        assert!(version_tuple("0.2.10") > version_tuple("0.2.3"));
        assert_eq!(version_tuple("0.2"), None);
        assert_eq!(version_tuple("0.2.3-beta"), None);
        assert_eq!(version_tuple("0.2.3.1"), None);
    }

    #[test]
    fn cleanup_updates_removes_only_update_executables() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let stale = dir.path().join("update-1234.exe");
        let keep_json = dir.path().join("recent.json");
        let keep_other = dir.path().join("update-notes.txt");
        fs::write(&stale, b"MZ")?;
        fs::write(&keep_json, b"[]")?;
        fs::write(&keep_other, b"notes")?;
        cleanup_updates(dir.path());
        assert!(!stale.exists());
        assert!(keep_json.exists());
        assert!(keep_other.exists());
        Ok(())
    }

    #[test]
    fn creates_windows_shortcut_files() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let exe = dir.path().join("HayakuReader.exe");
        fs::write(&exe, b"MZ")?;
        let desktop_lnk = dir.path().join("Desktop").join("HayakuReader.lnk");
        let start_lnk = dir.path().join("Programs").join("HayakuReader.lnk");
        run_shortcut_script(&exe, Some(&[&desktop_lnk, &start_lnk]))?;
        assert!(fs::metadata(&desktop_lnk)?.len() > 0);
        assert!(fs::metadata(&start_lnk)?.len() > 0);
        Ok(())
    }
}
