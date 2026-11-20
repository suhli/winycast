#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod controller;

use std::{cell::RefCell, ffi::OsString, path::PathBuf, rc::Rc};

use anyhow::{Context as _, Result, anyhow};
use gpui::*;
use tokio::runtime::Builder;
use winycast_platform_windows::{
    DesktopEvent, DesktopHost, Instance, Panel, enable_per_monitor_dpi,
};
use winycast_storage::{Storage, unix_time};
use winycast_ui::{CommandPalette, PANEL_HEIGHT, PANEL_WIDTH};

use controller::{Controller, Event};

fn main() {
    if let Err(error) = run() {
        eprintln!("Winycast: {error:#}");
        winycast_platform_windows::report_startup_error(&format!("{error:#}"));
        std::process::exit(1);
    }
}

fn data_path(arguments: &[OsString]) -> Result<PathBuf> {
    if let Some(index) = arguments.iter().position(|arg| arg == "--data-dir") {
        return Ok(PathBuf::from(
            arguments
                .get(index + 1)
                .context("--data-dir needs a directory")?,
        )
        .join("winycast.sqlite3"));
    }
    let local =
        std::env::var_os("LOCALAPPDATA").ok_or_else(|| anyhow!("LOCALAPPDATA is unavailable"))?;
    Ok(PathBuf::from(local)
        .join("Winycast")
        .join("winycast.sqlite3"))
}

fn run() -> Result<()> {
    let Some(_instance) = Instance::acquire()? else {
        return Ok(());
    };
    enable_per_monitor_dpi();
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let show_on_start = arguments.iter().any(|arg| arg == "--show");
    let trace = arguments.iter().any(|arg| arg == "--trace-window");
    let inspect_window =
        cfg!(debug_assertions) && arguments.iter().any(|arg| arg == "--inspect-window");
    let path = data_path(&arguments)?;
    let runtime = Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(6)
        .enable_time()
        .build()?;
    let (storage, settings) = runtime.block_on(runtime.spawn_blocking(move || {
        let storage =
            Storage::open(&path).with_context(|| format!("Could not open {}", path.display()))?;
        let settings = storage.settings()?;
        storage.expire_clips(unix_time(), settings.clipboard_days)?;
        Ok::<_, anyhow::Error>((storage, settings))
    }))??;

    let status = Rc::new(RefCell::new(Ok(())));
    let startup_status = status.clone();
    let handle = runtime.handle().clone();
    Application::new().run(move |cx| {
        if let Err(error) = (|| -> Result<()> {
            let (desktop_tx, desktop_rx) = async_channel::unbounded();
            let (intent_tx, intent_rx) = async_channel::unbounded();
            let (worker_tx, worker_rx) = async_channel::unbounded();
            let host = DesktopHost::new(desktop_tx.clone(), settings.hotkey)?;
            let window = cx.open_window(
                WindowOptions {
                    window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
                        None,
                        size(px(PANEL_WIDTH as f32), px(PANEL_HEIGHT as f32)),
                        cx,
                    ))),
                    titlebar: None,
                    window_background: WindowBackgroundAppearance::Transparent,
                    kind: if inspect_window {
                        WindowKind::Normal
                    } else {
                        WindowKind::PopUp
                    },
                    focus: false,
                    show: false,
                    is_resizable: false,
                    is_movable: false,
                    is_minimizable: false,
                    ..Default::default()
                },
                |window, cx| {
                    window.set_window_title("Winycast");
                    cx.new(|cx| CommandPalette::new(intent_tx, settings.clone(), cx))
                },
            )?;
            let panel = window.update(cx, |_, window, _| Panel::attach(window))??;
            let mut controller =
                Controller::new(panel, host, storage, settings, handle, worker_tx, trace);
            controller.initialize();
            if show_on_start {
                let _ = desktop_tx.try_send(DesktopEvent::Show);
            }
            cx.spawn(async move |cx| {
                use futures_lite::future::race;
                loop {
                    let event = race(
                        async { desktop_rx.recv().await.ok().map(Event::Desktop) },
                        race(
                            async { intent_rx.recv().await.ok().map(Event::Ui) },
                            async { worker_rx.recv().await.ok().map(Event::Worker) },
                        ),
                    )
                    .await;
                    let Some(event) = event else {
                        break;
                    };
                    if matches!(event, Event::Desktop(DesktopEvent::Quit)) {
                        let _ = cx.update(|cx| cx.quit());
                        break;
                    }
                    if window
                        .update(cx, |ui, window, cx| {
                            controller.handle(event, ui, window, cx)
                        })
                        .is_err()
                    {
                        break;
                    }
                }
            })
            .detach();
            Ok(())
        })() {
            *startup_status.borrow_mut() = Err(error);
            cx.quit();
        }
    });
    status.replace(Ok(()))
}
