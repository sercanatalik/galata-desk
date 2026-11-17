//! galata-desk: a native, read-only view of a Hyperliquid account's portfolio.

mod connect;
mod desk;
mod grid;
mod market_tile;
mod markets;
mod portfolio;
#[cfg(feature = "shot")]
mod shot;
mod theme;
mod ui;
#[cfg(test)]
mod ui_tests;

use std::sync::Arc;

use galata_desk_core::store::Store;
use gpui_kit::component::TitleBar;
use gpui_kit::prelude::*;
use gpui_kit::{App, Bounds, Global, WindowBounds, WindowOptions, px, size};

/// What every screen shares: the runtime that does the I/O, and the app's
/// directory on disk.
pub struct Core {
    /// HTTP and the connection bridge run here; GPUI's own executor only waits
    /// on what they send back.
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub store: Store,
    /// The venue's info endpoint. `GALATA_DESK_INFO_URL` points the app at
    /// another one, for development.
    pub info_url: String,
    /// The venue's WebSocket endpoint for market data. `GALATA_DESK_WS_URL`
    /// points the app at another one, for development.
    pub ws_url: String,
}

impl Global for Core {}

impl Core {
    pub fn get(cx: &App) -> &Core {
        cx.global::<Core>()
    }
}

/// `GALATA_DESK_DIR` moves the app's directory, for development and tests.
fn store() -> Store {
    match std::env::var_os("GALATA_DESK_DIR") {
        Some(dir) => Store::at(dir),
        None => Store::default_location().expect("the system has an application data directory"),
    }
}

/// Give the running app its Dock icon.
///
/// A packaged app gets its icon from its bundle. This covers the app run as a
/// bare binary, and does no harm inside a bundle.
#[cfg(target_os = "macos")]
fn set_dock_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    const ICON: &[u8] = include_bytes!("../../../assets/icon.png");
    let Some(main_thread) = MainThreadMarker::new() else {
        return;
    };
    let data = NSData::with_bytes(ICON);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        // SAFETY: called on the main thread with a valid image.
        unsafe {
            NSApplication::sharedApplication(main_thread).setApplicationIconImage(Some(&image))
        };
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("galata-desk-io")
        .enable_all()
        .build()
        .expect("the I/O runtime starts");

    gpui_kit::application()
        .with_assets(theme::Assets)
        .run(move |cx| {
            gpui_kit::init(cx);
            theme::install(cx);
            #[cfg(target_os = "macos")]
            set_dock_icon();
            cx.set_global(Core {
                runtime: Arc::new(runtime),
                store: store(),
                info_url: std::env::var("GALATA_DESK_INFO_URL")
                    .unwrap_or_else(|_| galata_desk_core::venue::INFO_URL.to_string()),
                ws_url: std::env::var("GALATA_DESK_WS_URL")
                    .unwrap_or_else(|_| galata_desk_core::market::ws::WS_URL.to_string()),
            });

            // `GALATA_DESK_SIZE=1100x700` opens the window at another size.
            let (width, height) = std::env::var("GALATA_DESK_SIZE")
                .ok()
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or((1320., 860.));
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            // A screenshot run draws offscreen: its window stays hidden and
            // takes no focus, so it cannot get in the way of other work.
            let quiet = cfg!(feature = "shot") && std::env::var_os("GALATA_DESK_SHOT").is_some();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(1080.), px(680.))),
                show: !quiet,
                focus: !quiet,
                ..TitleBar::window_options()
            };
            let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
                cx.new(|cx| desk::Desk::new(window, cx))
            })
            .expect("the main window opens");
            #[cfg(feature = "shot")]
            shot::arm(window, cx);
            #[cfg(not(feature = "shot"))]
            let _ = window;
            if !quiet {
                cx.activate(true);
            }
        });
}
