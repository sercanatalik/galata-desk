//! galata-desk: a native, read-only view of a Hyperliquid account's portfolio.

mod connect;
mod desk;
mod funding;
mod grid;
mod history;
mod level_form;
mod market_about;
mod market_position;
mod market_tile;
mod markets;
mod portfolio;
mod settings;
#[cfg(feature = "shot")]
mod shot;
mod theme;
mod ui;
#[cfg(test)]
mod ui_tests;
mod watcher;
mod watcher_link;

use std::sync::Arc;

use galata_desk_core::network::{self, Network};
use galata_desk_core::secrets::Secrets;
use galata_desk_core::store::{QuickSizes, Store};
use gpui_kit::component::TitleBar;
use gpui_kit::prelude::*;
use gpui_kit::{App, Bounds, Global, WindowBounds, WindowOptions, px, size};

/// The smallest the window may be, in points. Tall enough that the Markets
/// panel (with its Buy and Sell buttons) and one whole row of tiles both fit
/// beneath the toolbar and legend.
pub const MIN_WINDOW_WIDTH: f32 = 1080.;
pub const MIN_WINDOW_HEIGHT: f32 = 768.;
/// The window opens at this size unless `GALATA_DESK_SIZE` says otherwise.
pub const DEFAULT_WINDOW_WIDTH: f32 = 1480.;
pub const DEFAULT_WINDOW_HEIGHT: f32 = 960.;

/// What every screen shares: the runtime that does the I/O, and the app's
/// directory on disk.
pub struct Core {
    /// HTTP and the connection bridge run here; GPUI's own executor only waits
    /// on what they send back.
    pub runtime: Arc<tokio::runtime::Runtime>,
    pub store: Store,
    /// Where secrets are kept: the system keychain.
    pub secrets: Arc<dyn Secrets>,
    /// Which of the venue's networks is shown. Chosen in settings.
    pub network: Network,
    /// The user has allowed order actions on the main network. Chosen in
    /// settings, off until then.
    pub allow_mainnet_trading: bool,
    /// The order ticket's Quick buttons. Chosen in settings; always a list
    /// that keeps every rule.
    pub quick_sizes: QuickSizes,
    /// Where galata-watcher answers, when one is connected in settings. Its
    /// token is read from `secrets` when needed, never kept here.
    pub watcher_url: Option<galata_desk_core::watcher::WatcherUrl>,
    /// `GALATA_DESK_INFO_URL` points the app at another info endpoint, for
    /// development. It is used whatever the network.
    pub info_override: Option<String>,
    /// `GALATA_DESK_WS_URL` does the same for the market data stream.
    pub ws_override: Option<String>,
}

impl Global for Core {}

impl Core {
    pub fn get(cx: &App) -> &Core {
        cx.global::<Core>()
    }

    fn endpoints(&self) -> network::Endpoints {
        network::endpoints(
            self.network,
            self.info_override.clone(),
            self.ws_override.clone(),
        )
    }

    /// The venue's info endpoint for the network in use.
    pub fn info_url(&self) -> String {
        self.endpoints().info_url
    }

    /// The venue's market data stream for the network in use.
    pub fn ws_url(&self) -> String {
        self.endpoints().ws_url
    }

    /// The addresses come from the development environment, not the network.
    pub fn overridden(&self) -> bool {
        self.endpoints().overridden
    }

    /// What is remembered across launches.
    pub fn settings(&self) -> galata_desk_core::store::Settings {
        galata_desk_core::store::Settings {
            network: self.network,
            allow_mainnet_trading: self.allow_mainnet_trading,
            quick_sizes: self.quick_sizes.clone(),
            watcher_url: self.watcher_url.clone(),
        }
    }
}

/// The system keychain. A screenshot run keeps its secrets in memory, so it
/// can never read or change real ones.
fn secrets() -> Arc<dyn Secrets> {
    #[cfg(all(target_os = "macos", not(feature = "shot")))]
    {
        Arc::new(galata_desk_core::secrets::Keychain::new())
    }
    #[cfg(not(all(target_os = "macos", not(feature = "shot"))))]
    {
        Arc::new(galata_desk_core::secrets::MemorySecrets::default())
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
            // The workspace's panels can be rebuilt from a saved layout.
            crate::markets::dock::register(cx);
            #[cfg(target_os = "macos")]
            set_dock_icon();
            let store = store();
            let settings = store.settings();
            cx.set_global(Core {
                runtime: Arc::new(runtime),
                store,
                secrets: secrets(),
                network: settings.network,
                allow_mainnet_trading: settings.allow_mainnet_trading,
                quick_sizes: settings.quick_sizes,
                watcher_url: settings.watcher_url,
                info_override: std::env::var("GALATA_DESK_INFO_URL").ok(),
                ws_override: std::env::var("GALATA_DESK_WS_URL").ok(),
            });

            // `GALATA_DESK_SIZE=1100x700` opens the window at another size.
            let (width, height) = std::env::var("GALATA_DESK_SIZE")
                .ok()
                .and_then(|v| {
                    let (w, h) = v.split_once('x')?;
                    Some((w.parse().ok()?, h.parse().ok()?))
                })
                .unwrap_or((DEFAULT_WINDOW_WIDTH, DEFAULT_WINDOW_HEIGHT));
            let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
            // A screenshot run draws offscreen: its window stays hidden and
            // takes no focus, so it cannot get in the way of other work.
            let quiet = cfg!(feature = "shot") && std::env::var_os("GALATA_DESK_SHOT").is_some();
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                window_min_size: Some(size(px(MIN_WINDOW_WIDTH), px(MIN_WINDOW_HEIGHT))),
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
