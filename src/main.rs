#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod flow;
mod highlight;
mod math;
mod pdf;
#[cfg(windows)]
mod platform;
mod storage;
mod theme;
mod word;

use std::{
    collections::VecDeque,
    error::Error,
    fs,
    io,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::Duration,
};
use uuid::Uuid;

#[cfg(windows)]
fn acquire_instance_lock(lock_path: &Path, wait_for_lock: bool) -> io::Result<Option<fs::File>> {
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    let attempts = if wait_for_lock { 100 } else { 1 };
    for attempt in 0..attempts {
        match lock.try_lock() {
            Ok(()) => return Ok(Some(lock)),
            Err(std::fs::TryLockError::WouldBlock) => {
                if attempt + 1 < attempts {
                    thread::sleep(Duration::from_millis(50));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(None)
}

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

#[cfg(windows)]
fn has_hardware_vulkan_adapter(base_desc: &eframe::wgpu::InstanceDescriptor) -> bool {
    let instance = eframe::wgpu::Instance::new(eframe::wgpu::InstanceDescriptor {
        backends: eframe::wgpu::Backends::VULKAN,
        flags: base_desc.flags,
        memory_budget_thresholds: base_desc.memory_budget_thresholds,
        backend_options: base_desc.backend_options.clone(),
        display: None,
    });
    let mut fut = std::pin::pin!(instance.enumerate_adapters(eframe::wgpu::Backends::VULKAN));
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    match std::future::Future::poll(fut.as_mut(), &mut cx) {
        std::task::Poll::Ready(adapters) => adapters.iter().any(|adapter| {
            matches!(
                adapter.get_info().device_type,
                eframe::wgpu::DeviceType::IntegratedGpu | eframe::wgpu::DeviceType::DiscreteGpu
            )
        }),
        std::task::Poll::Pending => false,
    }
}

/// Configures wgpu to prefer the integrated/low-power adapter by default so hybrid-graphics
/// laptops do not wake the discrete GPU for 2D document reading while respecting `WGPU_POWER_PREF`.
///
/// When `WGPU_BACKEND` is unset and a hardware Vulkan adapter is available, restricts backends
/// to `VULKAN` because `egui-wgpu` keeps `available_adapters` alive in `RenderState` and
/// `wgpu-hal`'s DX12 adapter enumeration holds an open `ID3D12Device` per adapter (waking the
/// discrete GPU on Optimus laptops even when `LowPower` selects the iGPU), whereas Vulkan
/// adapters only hold a `vk::PhysicalDevice`.
#[cfg(windows)]
fn default_wgpu_options() -> eframe::egui_wgpu::WgpuConfiguration {
    let mut wgpu_setup = eframe::egui_wgpu::WgpuSetupCreateNew::without_display_handle();
    wgpu_setup.power_preference = eframe::wgpu::PowerPreference::from_env()
        .unwrap_or(eframe::wgpu::PowerPreference::LowPower);
    if eframe::wgpu::Backends::from_env().is_none()
        && has_hardware_vulkan_adapter(&wgpu_setup.instance_descriptor)
    {
        wgpu_setup.instance_descriptor.backends = eframe::wgpu::Backends::VULKAN;
    }
    eframe::egui_wgpu::WgpuConfiguration {
        wgpu_setup: eframe::egui_wgpu::WgpuSetup::CreateNew(wgpu_setup),
        ..Default::default()
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    #[cfg(not(windows))]
    return Err("HayakuReader 原生桌面版当前面向 Windows 平台。".into());

    #[cfg(windows)]
    {
        let mut launch_paths = VecDeque::new();
        let mut wait_for_lock = false;
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
            if argument == "--relaunch" {
                wait_for_lock = true;
                continue;
            }
            let path = PathBuf::from(argument);
            launch_paths.push_back(std::path::absolute(&path)?);
        }
        if platform::check_manual_update(&data)? {
            return Ok(());
        }
        let Some(instance_lock) = acquire_instance_lock(&data.join("instance.lock"), wait_for_lock)? else {
            let inbox = data.join("inbox");
            fs::create_dir_all(&inbox)?;
            storage::write_json(
                &inbox.join(format!("{}.json", Uuid::new_v4())),
                &launch_paths,
            )?;
            platform::allow_set_foreground_window();
            return Ok(());
        };

        platform::cleanup_updates(&data);
        let _ = platform::sync_installed_associations(&data);
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
            wgpu_options: default_wgpu_options(),
            ..Default::default()
        };
        eframe::run_native(
            "HayakuReader",
            options,
            Box::new(move |cc| {
                Ok(Box::new(app::HayakuApp::new(
                    cc,
                    data,
                    initial,
                    launch_paths,
                    Some(instance_lock),
                )))
            }),
        )
        .map_err(|e| format!("无法创建原生窗口：{e}"))?;
        Ok(())
    }
}

#[cfg(all(test, windows))]
#[test]
fn acquire_instance_lock_waits_when_relaunch_requested() -> io::Result<()> {
    let dir = tempfile::tempdir()?;
    let lock_path = dir.path().join("instance.lock");
    let first = acquire_instance_lock(&lock_path, false)?.expect("first lock should succeed");
    assert!(
        acquire_instance_lock(&lock_path, false)?.is_none(),
        "non-waiting acquisition should return None immediately"
    );
    let handle = thread::spawn(move || {
        thread::sleep(Duration::from_millis(120));
        let _ = first.unlock();
        drop(first);
    });
    let reacquired = acquire_instance_lock(&lock_path, true)?;
    handle.join().expect("thread join");
    assert!(reacquired.is_some(), "relaunch acquisition should wait and succeed");
    Ok(())
}

#[cfg(all(test, windows, target_env = "msvc"))]
#[test]
fn rendered_svg_app_icon_is_valid() {
    let icon = app::render_app_icon().expect("logo.svg icon");
    assert_eq!(icon.width, 64);
    assert_eq!(icon.height, 64);
    assert_eq!(icon.rgba.len(), 64 * 64 * 4);
}

#[cfg(all(test, windows))]
#[test]
fn default_wgpu_options_prefers_low_power_by_default() {
    match default_wgpu_options().wgpu_setup {
        eframe::egui_wgpu::WgpuSetup::CreateNew(setup) => {
            if std::env::var_os("WGPU_POWER_PREF").is_none() {
                assert_eq!(setup.power_preference, eframe::wgpu::PowerPreference::LowPower);
            }
            if std::env::var_os("WGPU_BACKEND").is_none()
                && has_hardware_vulkan_adapter(&setup.instance_descriptor)
            {
                assert_eq!(
                    setup.instance_descriptor.backends,
                    eframe::wgpu::Backends::VULKAN
                );
            }
        }
        other => panic!("expected WgpuSetup::CreateNew, got {other:?}"),
    }
}
