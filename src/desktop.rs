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
    window::WindowBuilder,
};
use wry::{DragDropEvent, WebContext, WebViewBuilder};

enum DesktopEvent {
    Dropped(PathBuf),
    Saved,
}

pub fn run(url: &str, app: Shared) -> Result<(), Box<dyn Error>> {
    let mut events = EventLoopBuilder::<DesktopEvent>::with_user_event().build();
    let window = WindowBuilder::new()
        .with_title("HayakuReader")
        .with_inner_size(LogicalSize::new(1150.0, 820.0))
        .with_min_inner_size(LogicalSize::new(560.0, 430.0))
        .build(&events)?;
    let dropped = events.create_proxy();
    let saved = events.create_proxy();
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
            if request.body() == "saved" {
                let _ = saved.send_event(DesktopEvent::Saved);
            }
        })
        .with_drag_drop_handler(move |event| {
            if let DragDropEvent::Drop { paths, .. } = event
                && let Some(path) = paths.into_iter().next()
            {
                let _ = dropped.send_event(DesktopEvent::Dropped(path));
            }
            true
        })
        .build(&window)?;
    let mut closing = None;
    events.run_return(|event, _, flow| {
        *flow = closing.map_or(ControlFlow::Wait, ControlFlow::WaitUntil);
        match event {
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            } if closing.is_none() => {
                let _ = webview.evaluate_script("window.hayakuFlush?.()");
                let deadline = Instant::now() + Duration::from_secs(2);
                closing = Some(deadline);
                *flow = ControlFlow::WaitUntil(deadline);
            }
            Event::UserEvent(DesktopEvent::Saved) if closing.is_some() => *flow = ControlFlow::Exit,
            Event::UserEvent(DesktopEvent::Dropped(path)) if closing.is_none() => {
                if let Ok(mut pending) = app.dropped.lock() {
                    *pending = Some(path);
                }
                let _ = webview.evaluate_script("window.hayakuOpenDrop?.()");
            }
            Event::NewEvents(_) if closing.is_some_and(|deadline| Instant::now() >= deadline) => {
                *flow = ControlFlow::Exit;
            }
            _ => {}
        }
    });
    Ok(())
}
