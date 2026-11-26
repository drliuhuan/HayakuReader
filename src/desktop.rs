use crate::Shared;
use std::{
    error::Error,
    path::PathBuf,
    time::{Duration, Instant},
};
use tao::{
    dpi::LogicalSize,
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder},
    platform::run_return::EventLoopExtRunReturn,
    platform::windows::{IconExtWindows, WindowBuilderExtWindows},
    window::{Icon, ResizeDirection, WindowBuilder},
};
use wry::{DragDropEvent, WebContext, WebViewBuilder};

enum DesktopEvent {
    Dropped(PathBuf),
    Saved,
    Restart,
    Drag,
    Resize(ResizeDirection),
    Minimize,
    ToggleMaximize,
    Close,
    Poll,
}

pub fn run(url: &str, app: Shared) -> Result<(), Box<dyn Error>> {
    let mut events = EventLoopBuilder::<DesktopEvent>::with_user_event().build();
    let icon = Icon::from_resource(1, None).ok();
    let window = WindowBuilder::new()
        .with_title("HayakuReader")
        .with_window_icon(icon.clone())
        .with_taskbar_icon(icon)
        .with_decorations(false)
        .with_undecorated_shadow(true)
        .with_inner_size(LogicalSize::new(1150.0, 820.0))
        .with_min_inner_size(LogicalSize::new(560.0, 430.0))
        .build(&events)?;
    let dropped = events.create_proxy();
    let ipc = events.create_proxy();
    let local = url
        .split('#')
        .next()
        .unwrap_or(url)
        .split('?')
        .next()
        .unwrap_or(url)
        .to_owned();
    let mut context = WebContext::new(Some(app.data.join("webview")));
    let webview = WebViewBuilder::new_with_web_context(&mut context)
        .with_url(url)
        .with_navigation_handler(move |url| {
            url == "about:blank"
                || url == local
                || url.starts_with(&(local.clone() + "?"))
                || url.starts_with(&(local.clone() + "#"))
        })
        .with_ipc_handler(move |request| {
            let cmd = match request.body().as_str() {
                "saved" => Some(DesktopEvent::Saved),
                "restart" => Some(DesktopEvent::Restart),
                "drag" => Some(DesktopEvent::Drag),
                "minimize" => Some(DesktopEvent::Minimize),
                "maximize" => Some(DesktopEvent::ToggleMaximize),
                "close" => Some(DesktopEvent::Close),
                "resize:n" => Some(DesktopEvent::Resize(ResizeDirection::North)),
                "resize:nw" => Some(DesktopEvent::Resize(ResizeDirection::NorthWest)),
                "resize:ne" => Some(DesktopEvent::Resize(ResizeDirection::NorthEast)),
                _ => None,
            };
            if let Some(event) = cmd {
                let _ = ipc.send_event(event);
            }
        })
        .with_drag_drop_handler(move |event| {
            if let DragDropEvent::Drop { paths, .. } = event {
                for path in paths {
                    let _ = dropped.send_event(DesktopEvent::Dropped(path));
                }
            }
            true
        })
        .build(&window)?;
    let mut closing = None;
    let mut last_maximized = window.is_maximized();
    // A proxy tick wakes the native message loop even while WebView2 is idle.
    let poll = events.create_proxy();
    let (stop_poll, stopped) = std::sync::mpsc::channel::<()>();
    let poller = std::thread::spawn(move || {
        while matches!(
            stopped.recv_timeout(Duration::from_millis(350)),
            Err(std::sync::mpsc::RecvTimeoutError::Timeout)
        ) {
            if poll.send_event(DesktopEvent::Poll).is_err() {
                break;
            }
        }
    });
    events.run_return(|event, _, flow| {
        *flow = ControlFlow::Wait;
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(DesktopEvent::Close)
                if closing.is_none() =>
            {
                let _ = webview.evaluate_script("window.hayakuFlush?.()");
                let deadline = Instant::now() + Duration::from_secs(2);
                closing = Some(deadline);
                *flow = ControlFlow::WaitUntil(deadline);
            }
            Event::WindowEvent {
                event: WindowEvent::Resized(_),
                ..
            } => {
                let maximized = window.is_maximized();
                if maximized != last_maximized {
                    last_maximized = maximized;
                    let script = if maximized {
                        "window.hayakuMaximized?.(true)"
                    } else {
                        "window.hayakuMaximized?.(false)"
                    };
                    let _ = webview.evaluate_script(script);
                }
            }
            Event::UserEvent(DesktopEvent::Drag) if closing.is_none() => {
                let _ = window.drag_window();
            }
            Event::UserEvent(DesktopEvent::Resize(direction))
                if closing.is_none() && !window.is_maximized() =>
            {
                let _ = window.drag_resize_window(direction);
            }
            Event::UserEvent(DesktopEvent::Minimize) if closing.is_none() => {
                window.set_minimized(true);
            }
            Event::UserEvent(DesktopEvent::ToggleMaximize) if closing.is_none() => {
                window.set_maximized(!window.is_maximized());
            }
            Event::UserEvent(DesktopEvent::Saved) if closing.is_some() => *flow = ControlFlow::Exit,
            Event::UserEvent(DesktopEvent::Restart)
                if app.restart.lock().is_ok_and(|pending| pending.is_some()) =>
            {
                *flow = ControlFlow::Exit
            }
            Event::UserEvent(DesktopEvent::Dropped(path)) if closing.is_none() => {
                if let Ok(mut pending) = app.dropped.lock() {
                    pending.push_back(path);
                }
                let _ = webview.evaluate_script("window.hayakuOpenDrop?.()");
            }
            Event::UserEvent(DesktopEvent::Poll)
                if closing.is_some_and(|deadline| Instant::now() >= deadline) =>
            {
                *flow = ControlFlow::Exit;
            }
            Event::UserEvent(DesktopEvent::Poll) if closing.is_none() => {
                if app.data.join("request-restart").is_file() {
                    let _ = std::fs::remove_file(app.data.join("request-restart"));
                    let _ = webview.evaluate_script("window.hayakuFlush?.()");
                    closing = Some(Instant::now() + Duration::from_secs(2));
                    *flow = ControlFlow::WaitUntil(closing.unwrap());
                }
                if let Ok(entries) = std::fs::read_dir(app.data.join("inbox")) {
                    for file in entries.flatten().filter(|entry| {
                        entry
                            .path()
                            .extension()
                            .is_some_and(|extension| extension == "json")
                    }) {
                        let path = file.path();
                        if let Ok(paths) = crate::read_json::<Vec<PathBuf>>(&path) {
                            let _ = std::fs::remove_file(path);
                            if let Ok(mut pending) = app.dropped.lock() {
                                pending.extend(paths);
                            }
                            window.set_focus();
                        }
                    }
                }
                if app.dropped.lock().is_ok_and(|paths| !paths.is_empty()) {
                    let _ = webview.evaluate_script("window.hayakuOpenDrop?.()");
                }
            }
            _ => {}
        }
    });
    drop(stop_poll);
    let _ = poller.join();
    Ok(())
}

#[cfg(all(test, target_env = "msvc"))]
#[test]
fn executable_contains_the_application_icon() {
    assert!(Icon::from_resource(1, None).is_ok());
}
