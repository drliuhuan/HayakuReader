#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod flow;
mod math;
mod pdf;
#[cfg(windows)]
mod platform;
mod storage;
mod theme;
mod word;

use std::{collections::VecDeque, error::Error, fs, path::PathBuf, sync::Arc};
use uuid::Uuid;

fn main() {
    if let Err(error) = run() {
        eprintln!("HayakuReader：{error}");
        #[cfg(windows)]
        if std::env::args_os().any(|argument| argument == "--apply-update") {
            platform::report_replace_failure(error.as_ref());
        } else {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Error)
                .set_title("HayakuReader")
                .set_description(format!("无法启动阅读器：{error}"))
                .show();
        }
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    #[cfg(not(windows))]
    return Err("HayakuReader 原生桌面版当前面向 Windows 平台。".into());

    #[cfg(windows)]
    {
        let mut launch_paths = VecDeque::new();
        let data = storage::app_data()?;
        fs::create_dir_all(&data)?;
        let mut args = std::env::args_os().skip(1);
        while let Some(argument) = args.next() {
            if argument == "--apply-update" {
                platform::apply_update(
                    &data,
                    &args.next().ok_or("Missing version")?.to_string_lossy(),
                    args,
                )?;
                return Ok(());
            }
            let path = PathBuf::from(argument);
            launch_paths.push_back(std::path::absolute(&path)?);
        }
        if platform::check_manual_update(&data)? {
            return Ok(());
        }
        let lock = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(data.join("instance.lock"))?;
        let _instance = match lock.try_lock() {
            Ok(()) => lock,
            Err(std::fs::TryLockError::WouldBlock) => {
                let inbox = data.join("inbox");
                fs::create_dir_all(&inbox)?;
                storage::write_json(
                    &inbox.join(format!("{}.json", Uuid::new_v4())),
                    &launch_paths,
                )?;
                return Ok(());
            }
            Err(error) => return Err(error.into()),
        };

        platform::cleanup_updates(&data);
        platform::record_version(&data)?;

        let initial = launch_paths.pop_front();
        let mut viewport = egui::ViewportBuilder::default()
            .with_decorations(false)
            .with_inner_size([1150.0, 820.0])
            .with_min_inner_size([600.0, 440.0])
            .with_drag_and_drop(true);
        if let Some(icon) = app::render_app_icon() {
            viewport = viewport.with_icon(Arc::new(icon));
        }
        let options = eframe::NativeOptions {
            viewport,
            ..Default::default()
        };
        eframe::run_native(
            "HayakuReader",
            options,
            Box::new(move |cc| Ok(Box::new(app::HayakuApp::new(cc, data, initial, launch_paths)))),
        )
        .map_err(|e| format!("无法创建原生窗口：{e}"))?;
        Ok(())
    }
}

#[cfg(all(test, windows, target_env = "msvc"))]
#[test]
fn rendered_svg_app_icon_is_valid() {
    let icon = app::render_app_icon().expect("logo.svg icon");
    assert_eq!(icon.width, 64);
    assert_eq!(icon.height, 64);
    assert_eq!(icon.rgba.len(), 64 * 64 * 4);
}
