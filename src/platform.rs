use crate::storage::{EXTENSIONS, read_json, write_json};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::windows::process::CommandExt,
    path::{Path, PathBuf},
    process::Command,
    sync::{Arc, Mutex},
    time::Duration,
};
use uuid::Uuid;
use winreg::{RegKey, enums::HKEY_CURRENT_USER};

pub const REPO: &str = "https://github.com/drliuhuan/HayakuReader";
const RELEASE_API: &str = "https://api.github.com/repos/drliuhuan/HayakuReader/releases/latest";
const PROG_ID: &str = "HayakuReader.Document";
const CAPABILITIES: &str = r"Software\HayakuReader\Capabilities";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub enum Restart {
    Installed,
    Update { executable: PathBuf },
}

#[derive(Clone, Debug)]
pub struct InstallationStatus {
    pub installed: bool,
    pub installation_exists: bool,
    pub path: PathBuf,
    pub associations: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub name: String,
    pub browser_download_url: String,
    pub digest: Option<String>,
    pub size: u64,
}

#[derive(Clone, Debug)]
pub struct DownloadProgress {
    pub downloaded: u64,
    pub total: u64,
    pub bytes_per_second: u64,
}

pub fn installed_path() -> io::Result<PathBuf> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or(io::ErrorKind::NotFound)?;
    Ok(PathBuf::from(local).join("Programs/HayakuReader/HayakuReader.exe"))
}

pub fn installed() -> bool {
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
    let clean = version.trim_start_matches('v').split('-').next()?;
    let mut parts = clean.split('.');
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
    EXTENSIONS
        .iter()
        .filter(|extension| !matches!(**extension, "txt" | "doc" | "docx"))
        .map(|extension| (*extension).to_owned())
        .collect()
}

fn associations(data: &Path) -> io::Result<Vec<String>> {
    let path = data.join("associations.json");
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

fn register(data: &Path, selected: &[String]) -> io::Result<()> {
    let exe = installed_path()?;
    if !exe.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "HayakuReader is not installed",
        ));
    }
    let root = RegKey::predef(HKEY_CURRENT_USER);
    register_associations(&root, data, &exe, selected)?;
    #[link(name = "shell32")]
    unsafe extern "system" {
        fn SHChangeNotify(
            event: i32,
            flags: u32,
            first: *const core::ffi::c_void,
            second: *const core::ffi::c_void,
        );
    }
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
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
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
    for extension in EXTENSIONS {
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

pub fn validate_external_url(raw: &str) -> Result<url::Url, &'static str> {
    let parsed = url::Url::parse(raw.trim()).map_err(|_| "链接格式无效")?;
    match parsed.scheme() {
        "http" | "https" if parsed.has_host() => Ok(parsed),
        "mailto" if !parsed.path().is_empty() => Ok(parsed),
        _ => Err("仅支持打开 http、https 或 mailto 外部链接"),
    }
}

pub fn open_external_url(raw: &str) -> Result<(), String> {
    let parsed = validate_external_url(raw).map_err(str::to_owned)?;
    shell_open(parsed.as_str())
}

pub fn shell_open(address: &str) -> Result<(), String> {
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
        return Err("无法在外部浏览器中打开链接".to_owned());
    }
    Ok(())
}

pub fn set_clipboard_png(png: &[u8]) -> Result<(), String> {
    const GMEM_MOVEABLE: u32 = 0x0002;
    const PNG_SIG: &[u8] = b"\x89PNG\r\n\x1a\n";
    if png.is_empty() || !png.starts_with(PNG_SIG) {
        return Err("图像数据无效".to_owned());
    }
    #[link(name = "user32")]
    unsafe extern "system" {
        fn OpenClipboard(hwnd: *const core::ffi::c_void) -> i32;
        fn EmptyClipboard() -> i32;
        fn SetClipboardData(
            format: u32,
            mem: *mut core::ffi::c_void,
        ) -> *mut core::ffi::c_void;
        fn CloseClipboard() -> i32;
        fn RegisterClipboardFormatW(name: *const u16) -> u32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GlobalAlloc(flags: u32, bytes: usize) -> *mut core::ffi::c_void;
        fn GlobalLock(mem: *mut core::ffi::c_void) -> *mut u8;
        fn GlobalUnlock(mem: *mut core::ffi::c_void) -> i32;
        fn GlobalFree(mem: *mut core::ffi::c_void) -> *mut core::ffi::c_void;
    }
    let png_name: Vec<u16> = "PNG\0".encode_utf16().collect();
    unsafe {
        let png_fmt = RegisterClipboardFormatW(png_name.as_ptr());
        if png_fmt == 0 {
            return Err("无法注册剪贴板 PNG 格式".to_owned());
        }
        if OpenClipboard(std::ptr::null()) == 0 {
            return Err("无法打开系统剪贴板".to_owned());
        }
        let result = (|| {
            if EmptyClipboard() == 0 {
                return Err("无法清空系统剪贴板".to_owned());
            }
            let hmem = GlobalAlloc(GMEM_MOVEABLE, png.len());
            if hmem.is_null() {
                return Err("无法分配剪贴板内存".to_owned());
            }
            let ptr = GlobalLock(hmem);
            if ptr.is_null() {
                GlobalFree(hmem);
                return Err("无法锁定剪贴板内存".to_owned());
            }
            std::ptr::copy_nonoverlapping(png.as_ptr(), ptr, png.len());
            GlobalUnlock(hmem);
            if SetClipboardData(png_fmt, hmem).is_null() {
                GlobalFree(hmem);
                return Err("写入剪贴板图像失败".to_owned());
            }
            Ok(())
        })();
        CloseClipboard();
        result
    }
}

pub fn installation_status(data: &Path) -> io::Result<InstallationStatus> {
    let path = installed_path()?;
    Ok(InstallationStatus {
        installed: installed(),
        installation_exists: path.is_file(),
        path,
        associations: associations(data)?,
    })
}

pub fn open_windows_defaults() -> Result<(), String> {
    if !installed() {
        return Err("请先安装 HayakuReader。".to_owned());
    }
    shell_open("ms-settings:defaultapps?registeredAppUser=HayakuReader")
}

pub fn apply_installation(
    data: &Path,
    extensions: &[String],
    install: bool,
) -> Result<Option<Restart>, String> {
    if extensions
        .iter()
        .any(|ext| !EXTENSIONS.contains(&ext.as_str()))
    {
        return Err("包含无效的文件扩展名".to_owned());
    }
    if install && !installed() {
        let target = installed_path().map_err(|e| e.to_string())?;
        if target.exists() {
            return Err("HayakuReader 已安装。".to_owned());
        }
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        copy_binary(&exe, &target).map_err(|e| e.to_string())?;
        write_json(&data.join("installed-version.json"), &VERSION).map_err(|e| e.to_string())?;
        register(data, extensions).map_err(|e| e.to_string())?;
        create_shortcuts(&target).map_err(|e| e.to_string())?;
        return Ok(Some(Restart::Installed));
    }
    if !installed() {
        return Err("请先安装 HayakuReader 再修改文件关联。".to_owned());
    }
    register(data, extensions).map_err(|e| e.to_string())?;
    Ok(None)
}

fn http(timeout: Duration) -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .user_agent("HayakuReader")
        .build()
        .into()
}

pub fn check_latest_release() -> Result<Option<Release>, String> {
    let check = || -> Result<Option<Release>, String> {
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
    };
    check().or_else(|_| check())
}

pub fn download_and_stage_update(
    data: &Path,
    release: &Release,
    progress_slot: &Arc<Mutex<Option<DownloadProgress>>>,
) -> Result<Restart, String> {
    if !installed() {
        return Err("请先安装 HayakuReader 再应用更新。".to_owned());
    }
    let asset = release
        .assets
        .iter()
        .find(|asset| asset.name == "HayakuReader.exe")
        .ok_or_else(|| "该发布版本没有 Windows 可执行文件".to_owned())?;
    let expected = format!(
        "{REPO}/releases/download/{}/HayakuReader.exe",
        release.tag_name
    );
    if asset.browser_download_url != expected {
        return Err("发布文件下载地址异常".to_owned());
    }
    let hash = asset
        .digest
        .as_deref()
        .and_then(|digest| digest.strip_prefix("sha256:"))
        .ok_or_else(|| "该发布版本缺少 SHA-256 校验值".to_owned())?;

    {
        let mut slot = progress_slot.lock().map_err(|_| "锁状态异常".to_owned())?;
        if slot.is_some() {
            return Err("更新正在下载中".to_owned());
        }
        *slot = Some(DownloadProgress {
            downloaded: 0,
            total: asset.size,
            bytes_per_second: 0,
        });
    }

    let result = (|| {
        let mut response = http(Duration::from_secs(180))
            .get(expected)
            .call()
            .map_err(|e| e.to_string())?;
        let mut reader = response
            .body_mut()
            .with_config()
            .limit(128 * 1024 * 1024)
            .reader();
        let mut file = tempfile::NamedTempFile::new_in(data).map_err(|e| e.to_string())?;
        let mut checksum = Sha256::new();
        let mut buffer = [0_u8; 64 * 1024];
        let mut downloaded = 0u64;
        let mut sampled_bytes = 0u64;
        let mut sampled_at = std::time::Instant::now();
        let mut signature = Vec::<u8>::with_capacity(2);
        loop {
            let count = reader.read(&mut buffer).map_err(|e| e.to_string())?;
            if count == 0 {
                break;
            }
            if signature.len() < 2 {
                signature.extend_from_slice(&buffer[..count.min(2 - signature.len())]);
            }
            checksum.update(&buffer[..count]);
            file.write_all(&buffer[..count]).map_err(|e| e.to_string())?;
            downloaded += count as u64;
            let elapsed = sampled_at.elapsed();
            if let Ok(mut slot) = progress_slot.lock()
                && let Some(progress) = slot.as_mut()
            {
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
            return Err("更新文件校验失败".to_owned());
        }
        let exe = data.join(format!("update-{}.exe", Uuid::new_v4()));
        file.as_file().sync_all().map_err(|e| e.to_string())?;
        file.persist(&exe).map_err(|e| e.to_string())?;
        Ok(Restart::Update { executable: exe })
    })();
    if let Ok(mut slot) = progress_slot.lock() {
        *slot = None;
    }
    result
}

pub fn restart(request: Restart) -> io::Result<()> {
    match request {
        Restart::Installed => {
            Command::new(installed_path()?).spawn()?;
        }
        Restart::Update { executable } => {
            Command::new(executable)
                .args(["--apply-update", VERSION])
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

pub fn report_replace_failure(error: &dyn std::error::Error) {
    let answer = rfd::MessageDialog::new()
        .set_title("HayakuReader")
        .set_level(rfd::MessageLevel::Error)
        .set_description(format!(
            "更新失败 / Update failed\n{error}\n\n打开 GitHub Release 手动下载？ / Open GitHub Releases to download manually?\n{REPO}/releases/latest"
        ))
        .set_buttons(rfd::MessageButtons::YesNo)
        .show();
    if answer == rfd::MessageDialogResult::Yes {
        let _ = shell_open(&format!("{REPO}/releases/latest"));
    }
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
            for ext in [".txt", ".doc", ".docx"] {
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
        assert!(version_tuple("0.3.0-alpha.1") > version_tuple("0.2.7"));
        assert_eq!(version_tuple("0.2"), None);
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

    #[test]
    fn validates_external_link_schemes() {
        assert!(validate_external_url("https://example.com/path?a=1").is_ok());
        assert!(validate_external_url(" http://example.com ").is_ok());
        assert!(validate_external_url("mailto:reader@example.com").is_ok());
        assert!(validate_external_url("javascript:alert(1)").is_err());
        assert!(validate_external_url("file:///C:/Windows/win.ini").is_err());
        assert!(validate_external_url(r"C:\Windows\notepad.exe").is_err());
        assert!(validate_external_url("http://").is_err());
    }
}
