//! The window, driven by real clicks in a headless window.
//!
//! The venue is pointed at a closed local port, so nothing here touches the
//! network: these tests are about what the controls do, not what they show.

use std::sync::Arc;

use galata_desk_core::connect::Address;
use galata_desk_core::store::{Account, Store};
use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
use gpui_kit::{
    AppContext, Bounds, Entity, Point, TestAppContext, WindowBounds, WindowHandle, WindowOptions,
    base::Root, px, size,
};

use std::sync::Arc as StdArc;

use galata_desk_core::market::MarketId;
use galata_desk_core::market::feed::{Listed, Listing, MarketSnapshot, Tile};
use galata_desk_core::market::watchlist::{LIMIT, Watchlist};
use galata_desk_core::model::PositionRow;
use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::venue::MarginMode;

use crate::Core;
use crate::desk::{Desk, Section};
use crate::market_position;
use crate::markets::Markets;

const ADDRESS: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";

fn open(
    cx: &mut TestAppContext,
    remembered: bool,
) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    open_sized(
        cx,
        remembered,
        size(
            px(crate::DEFAULT_WINDOW_WIDTH),
            px(crate::DEFAULT_WINDOW_HEIGHT),
        ),
    )
}

fn open_sized(
    cx: &mut TestAppContext,
    remembered: bool,
    window_size: gpui_kit::Size<gpui_kit::Pixels>,
) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let (window, desk) = open_at(cx, dir.path(), remembered, window_size);
    (window, desk, dir)
}

/// Open the app on a directory the test has prepared, as `main` does.
fn open_at(
    cx: &mut TestAppContext,
    dir: &std::path::Path,
    remembered: bool,
    window_size: gpui_kit::Size<gpui_kit::Pixels>,
) -> (WindowHandle<Root>, Entity<Desk>) {
    let store = Store::at(dir);
    if remembered {
        store
            .remember(&Account::proven_now(Address::parse(ADDRESS).unwrap()))
            .unwrap();
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(1)
        .enable_all()
        .build()
        .unwrap();
    let (window, desk) = cx.update(|cx| {
        gpui_kit::init(cx);
        crate::theme::install(cx);
        // The workspace's panels can be rebuilt from a saved layout.
        crate::markets::dock::register(cx);
        let stored = store.settings();
        let store_network = stored.network;
        cx.set_global(Core {
            runtime: Arc::new(runtime),
            store,
            // Tests never touch the real keychain.
            secrets: Arc::new(galata_desk_core::secrets::MemorySecrets::default()),
            network: store_network,
            allow_mainnet_trading: stored.allow_mainnet_trading,
            quick_sizes: stored.quick_sizes,
            watcher_url: stored.watcher_url,
            // Nothing listens here: every venue request fails at once,
            // whichever network is chosen.
            info_override: Some("http://127.0.0.1:9/info".into()),
            ws_override: Some("ws://127.0.0.1:9/ws".into()),
        });
        let (window, desk) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: window_size,
                })),
                ..Default::default()
            },
            cx,
            |window, cx| cx.new(|cx| Desk::new(window, cx)),
        )
        .unwrap();
        (window.downcast::<Root>().unwrap(), desk)
    });
    (window, desk)
}

#[gpui_kit::test]
fn with_no_account_the_connect_screen_is_shown(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("connect").is_some());
        assert!(window.try_find("disconnect").is_none());
        assert!(desk.read(cx).portfolio_view().is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_remembered_account_opens_its_portfolio_and_disconnect_forgets_it(cx: &mut TestAppContext) {
    let (window, desk, dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(desk.read(cx).portfolio_view().is_some());
        assert!(window.try_find("connect").is_none());

        window.click("disconnect", cx);
    })
    .unwrap();
    // The click's effect reaches the window after the update that made it.
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(desk.read(cx).portfolio_view().is_none());
        assert!(window.try_find("connect").is_some());
    })
    .unwrap();
    // The account is forgotten on disk too: a relaunch shows the connect screen.
    assert_eq!(Store::at(dir.path()).account(), None);
}

#[gpui_kit::test]
fn the_selectors_change_what_is_selected(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let view = desk.read(cx).portfolio_view().unwrap();
        assert_eq!(
            view.read(cx).selections(),
            "scope=All period=AllTime series=Pnl chart=Month fees=perps tab=positions"
        );

        // Each selector is a row of tabs, identified by position.
        window.within("scope").click(1usize, cx);
        window.within("period").click(1usize, cx);
        window.within("series").click(0usize, cx);
        window.within("chart-period").click(0usize, cx);
        window.within("fee-market").click(1usize, cx);
        window.within("holdings").click(2usize, cx);
        assert_eq!(
            view.read(cx).selections(),
            "scope=Perps period=Week series=AccountValue chart=Day fees=spot tab=orders"
        );

        window.within("holdings").click(0usize, cx);
        assert!(view.read(cx).selections().ends_with("tab=balances"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn no_control_on_the_portfolio_screen_trades(cx: &mut TestAppContext) {
    let (window, _desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // Every button the screen can show, by its accessible label.
        for forbidden in [
            "Close",
            "Close All",
            "Limit",
            "Market",
            "Reverse",
            "Trailing",
            "Deposit",
            "Withdraw",
            "Send",
            "Buy",
            "Sell",
        ] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "a `{forbidden}` control is on a read-only screen"
            );
        }
    })
    .unwrap();
}

// ---- The Markets section -------------------------------------------------

fn watchlist(coins: &[&str]) -> Watchlist {
    Watchlist::from_markets(coins.iter().map(|c| MarketId::new(*c)))
}

/// Open the window with a remembered account and, optionally, a watchlist.
fn open_with(
    cx: &mut TestAppContext,
    list: Option<&[&str]>,
) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    let (window, desk, dir) = open(cx, true);
    if let Some(coins) = list {
        Store::at(dir.path())
            .save_watchlist(&watchlist(coins))
            .unwrap();
    }
    (window, desk, dir)
}

/// Choose a period from the Markets period menu, by its name there.
fn choose_period(window: &mut gpui_kit::Window, name: &str, cx: &mut gpui_kit::App) {
    window.click("mperiod", cx);
    window.render_frame(cx);
    // The menu floats outside any element with an id: click where its item is.
    let item = window.find_by_label(name).bounds().center();
    window.pointer_move(item, None, cx);
    window.pointer_down(item, gpui_kit::MouseButton::Left, cx);
    window.pointer_up(item, gpui_kit::MouseButton::Left, cx);
    window.render_frame(cx);
}

/// Click Markets in the section switch and return the view.
fn go_to_markets(
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
    cx: &mut TestAppContext,
) -> Entity<Markets> {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("sections").click(1usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        desk.read(cx).markets_view().expect("Markets was opened")
    })
    .unwrap()
}

fn position(market: &str, dex: &str, value: i64) -> PositionRow {
    PositionRow {
        asset: None,
        market: market.into(),
        dex: dex.into(),
        leverage: 10,
        mode: MarginMode::Cross,
        size: Decimal::ONE,
        value: Decimal::from(value),
        entry: Decimal::ONE,
        mark: None,
        pnl: Decimal::ZERO,
        roe: Decimal::ZERO,
        liquidation: None,
        margin: Decimal::ZERO,
        funding: Decimal::ZERO,
    }
}

/// What the feed would report for these markets, with this list of markets
/// on offer.
fn snapshot(watched: &[&str], listed: &[&str]) -> MarketSnapshot {
    MarketSnapshot {
        tiles: watched
            .iter()
            .map(|coin| Tile {
                market: MarketId::new(*coin),
                live: None,
                stats: None,
                max_leverage: Some(10),
                unavailable: false,
            })
            .collect(),
        listing: Listing::Ready(StdArc::new(
            listed
                .iter()
                .map(|coin| Listed {
                    market: MarketId::new(*coin),
                    max_leverage: 10,
                })
                .collect(),
        )),
        ..MarketSnapshot::default()
    }
}

#[gpui_kit::test]
fn the_connect_screen_has_no_section_switch(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), None);
        assert!(window.find_all_by_label("Markets").is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_app_opens_on_portfolio_and_the_switch_shows_markets(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), Some(Section::Portfolio));
        // Markets, and its stream, do not exist until it is first chosen.
        assert!(desk.read(cx).markets_view().is_none());
    })
    .unwrap();
    go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(desk.read(cx).section(), Some(Section::Markets));
    })
    .unwrap();
}

#[gpui_kit::test]
fn each_sections_selections_survive_a_round_trip(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(markets.read(cx).selections(cx), "period=24H colour=Change");
        choose_period(window, "7 days", cx);
        window.within("colour").click(2usize, cx);
        assert_eq!(markets.read(cx).selections(cx), "period=7D colour=Funding");
        // To Portfolio, change something there, and back.
        window.within("sections").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), Some(Section::Portfolio));
        window.within("holdings").click(2usize, cx);
        window.within("sections").click(1usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selections(cx), "period=7D colour=Funding");
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        assert!(portfolio.read(cx).selections().ends_with("tab=orders"));
        // The same Markets view, not a new one.
        assert_eq!(desk.read(cx).markets_view().unwrap(), markets);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_period_and_colour_controls_each_select(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    for (name, period) in [
        ("3 minutes", "3m"),
        ("5 minutes", "5m"),
        ("15 minutes", "15m"),
        ("30 minutes", "30m"),
        ("1 hour", "1H"),
        ("24 hours", "24H"),
        ("7 days", "7D"),
    ] {
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            choose_period(window, name, cx);
            assert!(
                markets
                    .read(cx)
                    .selections(cx)
                    .starts_with(&format!("period={period} ")),
                "{name}"
            );
        })
        .unwrap();
        // The menu closes on its own turn.
        cx.run_until_parked();
    }
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        for (ix, colour) in ["Change", "Volatility", "Funding"].iter().enumerate() {
            window.within("colour").click(ix, cx);
            assert!(
                markets
                    .read(cx)
                    .selections(cx)
                    .ends_with(&format!("colour={colour}"))
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn clicking_a_tile_selects_its_market_and_pointing_does_not(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC", "ETH", "SOL"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // The first market is selected until another is chosen.
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));
        assert!(window.try_find("market-panel").is_some());

        // Pointing at tiles changes nothing but which one offers Remove.
        window.hover(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).hovered_market(cx).as_deref(), Some("ETH"));
        window.hover(("tile", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));

        // A click selects.
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));

        // Clicking the selected tile leaves it selected.
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));
    })
    .unwrap();
    // Remembered at once, with the list unchanged.
    let stored = Store::at(dir.path()).watchlist().unwrap();
    assert_eq!(stored.selected(), Some(&MarketId::new("ETH")));
    assert_eq!(stored.markets().len(), 3);
}

#[gpui_kit::test]
fn enter_or_space_on_a_focused_tile_selects_it(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH", "SOL"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // Focus alone does not select.
        markets.update(cx, |view, cx| view.focus_market("ETH", window, cx));
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));

        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("space", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("SOL"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn removing_the_selected_tile_selects_its_neighbour(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC", "ETH", "SOL"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        // Remove the selected tile with its button: the next one is selected.
        window.hover(("tile", 1usize), cx);
        window.render_frame(cx);
        window.click(("remove", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "SOL"]);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("SOL"));

        // Remove it again, now the last in the list: the one before it.
        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("delete", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));
        assert!(window.try_find("market-panel").is_some());
    })
    .unwrap();
    let stored = Store::at(dir.path()).watchlist().unwrap();
    assert_eq!(stored.selected(), Some(&MarketId::new("BTC")));
}

#[gpui_kit::test]
fn the_selection_is_restored_at_start(cx: &mut TestAppContext) {
    let (window, desk, dir) = open(cx, true);
    let mut list = watchlist(&["BTC", "ETH", "SOL"]);
    list.select(&MarketId::new("SOL"));
    Store::at(dir.path()).save_watchlist(&list).unwrap();
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("SOL"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_nothing_watched_there_is_no_panel(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(markets.read(cx).selected_market(cx), None);
        assert!(window.try_find("market-panel").is_none());
        assert!(window.try_find("add-first").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_tile_is_removed_by_its_button_or_from_the_keyboard(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC", "ETH", "SOL"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // The button shows while the tile is pointed at.
        window.hover(("tile", 1usize), cx);
        window.render_frame(cx);
        window.click(("remove", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "SOL"]);

        // From the keyboard: focus a tile and press Backspace.
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        window.press("backspace", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["SOL"]);
    })
    .unwrap();
    // Remembered at once.
    assert_eq!(Store::at(dir.path()).watchlist(), Some(watchlist(&["SOL"])));
}

#[gpui_kit::test]
fn removing_the_last_market_leaves_an_empty_remembered_list(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        window.press("delete", cx);
        window.render_frame(cx);
        assert!(markets.read(cx).watched(cx).is_empty());
        // The empty state offers the add action.
        assert!(window.try_find("add-first").is_some());
    })
    .unwrap();
    assert_eq!(
        Store::at(dir.path()).watchlist(),
        Some(Watchlist::default())
    );
}

#[gpui_kit::test]
fn a_market_is_added_by_search_and_not_added_twice(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    let listed = ["BTC", "ETH", "SOL", "xyz:TSLA", "flx:TSLA"];
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(snapshot(&["BTC"], &listed), cx)
        });
        window.render_frame(cx);
        window.click("add-market", cx);
        window.render_frame(cx);
        assert!(markets.read(cx).picker_is_open());

        markets.update(cx, |view, cx| view.set_search("tsl", window, cx));
        window.render_frame(cx);
        // Both TSLA markets match; BTC does not.
        assert!(window.try_find(("pick", 3usize)).is_some());
        assert!(window.try_find(("pick", 4usize)).is_some());
        assert!(window.try_find(("pick", 0usize)).is_none());

        // Choose the one on `xyz`: only that one is added.
        window.click(("pick", 3usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "xyz:TSLA"]);

        // It now reads as watched; choosing it again adds nothing.
        window.click(("pick", 3usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "xyz:TSLA"]);
    })
    .unwrap();
    assert_eq!(
        Store::at(dir.path()).watchlist(),
        Some(watchlist(&["BTC", "xyz:TSLA"]))
    );
}

#[gpui_kit::test]
fn a_full_watchlist_takes_no_more(cx: &mut TestAppContext) {
    let coins: Vec<String> = (0..LIMIT).map(|i| format!("M{i}")).collect();
    let refs: Vec<&str> = coins.iter().map(String::as_str).collect();
    let (window, desk, _dir) = open_with(cx, Some(&refs));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(snapshot(&refs, &["BTC", "ETH"]), cx)
        });
        window.render_frame(cx);
        window.click("add-market", cx);
        window.render_frame(cx);
        window.click(("pick", 0usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx).len(), LIMIT);
        assert!(!markets.read(cx).watched(cx).contains(&"BTC".to_string()));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_first_watchlist_is_what_the_account_holds(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, None);
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        // Positions have not loaded: nothing is decided yet.
        assert!(markets.read(cx).watched(cx).is_empty());
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_positions(
                vec![
                    position("BTC", "", 10_000),
                    position("ETH", "", 25_000),
                    position("NVDA", "xyz", 19_500),
                ],
                cx,
            )
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["ETH", "xyz:NVDA", "BTC"]);
    })
    .unwrap();
    // Remembered, so the next launch shows the same markets in the same order.
    assert_eq!(
        Store::at(dir.path()).watchlist(),
        Some(watchlist(&["ETH", "xyz:NVDA", "BTC"]))
    );
}

#[gpui_kit::test]
fn with_no_position_the_first_watchlist_is_four_majors(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, None);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| view.set_positions(Vec::new(), cx));
    })
    .unwrap();
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "ETH", "SOL", "HYPE"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_remembered_watchlist_is_shown_as_it_was_left(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["SOL", "xyz:NVDA", "BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // Even when the account holds other things.
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_positions(vec![position("ETH", "", 1)], cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["SOL", "xyz:NVDA", "BTC"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_nothing_held_the_ticket_is_the_markets_sections_only_trading_control(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        give_account(&desk, Some(vec![]), None, cx);
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        for forbidden in [
            "Trade", "Buy", "Sell", "Long", "Short", "Close", "Limit", "Market", "Deposit",
            "Withdraw", "Send",
        ] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "a `{forbidden}` control is in the Markets section"
            );
        }
        // Without a position or orders, the position panel offers nothing.
        for absent in [
            "Close BTC position",
            "Set TP / SL for BTC",
            "Cancel all BTC orders",
        ] {
            assert!(window.try_find_by_label(absent).is_none(), "{absent}");
        }
        // The ticket's two buttons are there.
        assert!(window.try_find_by_label("Buy BTC").is_some());
        assert!(window.try_find_by_label("Sell BTC").is_some());
        // Activating a tile does nothing but focus it.
        window.click(("tile", 0usize), cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "ETH"]);
        assert_eq!(desk.read(cx).section(), Some(Section::Markets));
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_held_market_adds_only_the_position_panels_reducing_controls(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, desk, _dir) = open_with(cx, Some(&["xyz:NVDA", "BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    let portfolio = cx
        .update_window(window.into(), |_, window, cx| {
            let portfolio = desk.read(cx).portfolio_view().unwrap();
            portfolio.update(cx, |p, cx| {
                p.use_trading(staged.clone());
                p.set_orders(vec![level(501, Tpsl::Tp, "160")], cx);
                p.set_positions(vec![long_nvda()], cx);
            });
            markets.update(cx, |view, cx| view.focus_market("xyz:NVDA", window, cx));
            markets.update(cx, |_, cx| cx.notify());
            window.render_frame(cx);
            portfolio
        })
        .unwrap();
    click(cx, window, "orders-header");
    cx.update_window(window.into(), |_, window, _| {
        for present in [
            "Close NVDA position",
            "Set TP / SL for NVDA",
            "Cancel all NVDA orders",
            "Buy NVDA",
            "Sell NVDA",
        ] {
            assert!(window.try_find_by_label(present).is_some(), "{present}");
        }
        // Nothing adds to the position or reverses it but the ticket.
        for forbidden in ["Add", "Increase", "Reverse", "Flip", "Long", "Short"] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "{forbidden}"
            );
        }
    })
    .unwrap();
    // Each opens a confirmation; nothing is sent without it.
    click(cx, window, "position-close");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(portfolio.read(cx).open_dialog(), Some("close-market"));
    })
    .unwrap();
    click(cx, window, "dialog-keep");
    click(cx, window, "position-tpsl");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(portfolio.read(cx).open_dialog(), Some("tpsl"));
    })
    .unwrap();
    click(cx, window, "dialog-keep");
    assert!(staged.closes.borrow().is_empty());
    assert!(staged.cancels.borrow().is_empty());
}

#[gpui_kit::test]
fn the_panel_is_detail_then_chart_then_position_and_the_detail_has_no_account_figures(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        give_account(&desk, Some(vec![position("BTC", "", 61_619)]), None, cx);
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);

        let detail = window.find("market-detail").bounds();
        let plot = window.find("chart-plot").bounds();
        let axis = window.find("chart-price-axis").bounds();
        let column = window.find("position-panel").bounds();
        assert!(detail.right() <= plot.left(), "{detail:?} {plot:?}");
        assert!(axis.right() <= column.left(), "{axis:?} {column:?}");

        // The size is the position panel's, not the detail's.
        let size = window.find("position-size").bounds();
        assert!(column.contains(&size.center()), "{size:?} {column:?}");
        assert!(!detail.contains(&size.center()));
        let model = markets.read(cx).position_panel(cx).unwrap();
        let market_position::Holding::Held(held) = model.holding else {
            panic!("BTC is held")
        };
        assert_eq!(held.size, "+1 BTC");
    })
    .unwrap();
}

#[gpui_kit::test]
fn at_the_smallest_window_the_three_columns_and_the_axes_all_show(cx: &mut TestAppContext) {
    let smallest = size(px(crate::MIN_WINDOW_WIDTH), px(crate::MIN_WINDOW_HEIGHT));
    let (window, desk, dir) = open_sized(cx, true, smallest);
    let coins = ["BTC", "ETH", "SOL", "HYPE", "DOGE"];
    Store::at(dir.path())
        .save_watchlist(&watchlist(&coins))
        .unwrap();
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let mut snapshot = charted("BTC", &coins, 40);
        snapshot.tiles = priced_snapshot(&coins).tiles;
        markets.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        give_account(&desk, Some(vec![position("BTC", "", 1_000)]), None, cx);
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);

        let panel = window.find("market-panel").bounds();
        let detail = window.find("market-detail").bounds();
        let plot = window.find("chart-plot").bounds();
        let axis = window.find("chart-price-axis").bounds();
        let times = window.find("chart-time-axis").bounds();
        let column = window.find("position-panel").bounds();
        let width = px(crate::MIN_WINDOW_WIDTH);
        assert!(column.right() <= width && column.right() <= panel.right());
        assert!(detail.left() >= panel.left());
        assert!(
            axis.right() <= column.left(),
            "the price axis is cut: {axis:?} {column:?}"
        );
        assert!(
            times.bottom() <= panel.bottom(),
            "the time axis is cut: {times:?} {panel:?}"
        );
        assert!(
            plot.size.width >= px(300.),
            "the chart is too narrow: {plot:?}"
        );
        let tiles = window.find("tiles").bounds();
        let first = window.find(("tile", 0usize)).bounds();
        assert!(first.bottom() <= tiles.bottom(), "no whole row of tiles");
    })
    .unwrap();
}

// ---- The account footer --------------------------------------------------

#[gpui_kit::test]
fn the_account_footer_shows_under_both_sections_and_not_before_connecting(cx: &mut TestAppContext) {
    let (window, _desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("account-footer").is_none());
    })
    .unwrap();

    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(
            window.try_find("account-footer").is_some(),
            "under Portfolio"
        );
    })
    .unwrap();
    go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("account-footer").is_some(), "under Markets");
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_account_summary_waits_for_both_holdings_and_history(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        // Nothing has loaded: no figures, so the footer shows placeholders.
        assert!(portfolio.read(cx).account_summary().is_none());
        // Positions alone are not enough: equity needs the history's side too.
        portfolio.update(cx, |view, cx| {
            view.set_positions(vec![position("ETH", "", 100)], cx)
        });
        assert!(portfolio.read(cx).account_summary().is_none());
    })
    .unwrap();
}

/// A snapshot whose tiles have a change over the period, in watchlist order.
fn moving(changes: &[(&str, f64)]) -> MarketSnapshot {
    use galata_desk_core::market::candles::Stats;
    let coins: Vec<&str> = changes.iter().map(|(coin, _)| *coin).collect();
    let mut snapshot = snapshot(&coins, &coins);
    for (tile, (_, change)) in snapshot.tiles.iter_mut().zip(changes) {
        tile.stats = Some(Stats {
            start: Decimal::ONE,
            change: *change,
            low: Decimal::ONE,
            high: Decimal::ONE,
            marker: 0.5,
            volatility: Some(1.0),
            mean: None,
            sd: None,
        });
    }
    snapshot
}

/// Choose an order from the Markets sort menu, by its name there.
fn choose_sort(window: &mut gpui_kit::Window, name: &str, cx: &mut gpui_kit::App) {
    window.click("msort", cx);
    window.render_frame(cx);
    let item = window.find_by_label(name).bounds().center();
    window.pointer_move(item, None, cx);
    window.pointer_down(item, gpui_kit::MouseButton::Left, cx);
    window.pointer_up(item, gpui_kit::MouseButton::Left, cx);
    window.render_frame(cx);
}

#[gpui_kit::test]
fn sorting_reorders_the_tiles_and_leaves_the_watchlist_and_selection_alone(
    cx: &mut TestAppContext,
) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC", "SOL", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", -0.7), ("SOL", 1.3), ("ETH", 0.2)]), cx)
        });
        window.render_frame(cx);
        // The section opens in watchlist order.
        assert_eq!(markets.read(cx).drawn(cx), ["BTC", "SOL", "ETH"]);

        choose_sort(window, "Change, gainers first", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["SOL", "ETH", "BTC"]);
        // Only the drawing order changed.
        assert_eq!(markets.read(cx).watched(cx), ["BTC", "SOL", "ETH"]);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // A tile keeps its identity wherever it is drawn: this is ETH's.
        window.click(("tile", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));
        choose_sort(window, "Change, losers first", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["BTC", "ETH", "SOL"]);
    })
    .unwrap();
    let stored = Store::at(dir.path()).watchlist().unwrap();
    let stored: Vec<String> = stored.markets().iter().map(|m| m.to_string()).collect();
    assert_eq!(stored, ["BTC", "SOL", "ETH"]);
}

#[gpui_kit::test]
fn a_sorted_order_settles_instead_of_following_every_tick(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "SOL", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", -0.7), ("SOL", 1.3), ("ETH", 0.2)]), cx)
        });
        window.render_frame(cx);
        choose_sort(window, "Change, gainers first", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["SOL", "ETH", "BTC"]);

        // BTC overtakes: within the interval the tiles stay where they are.
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", 2.0), ("SOL", 1.3), ("ETH", 0.2)]), cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(cx), ["SOL", "ETH", "BTC"]);

        // Once the interval has passed, the order catches up.
        markets.update(cx, |view, cx| {
            view.age_order(cx);
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(cx), ["BTC", "SOL", "ETH"]);

        // A market removed is gone from the order at once.
        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("delete", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(cx), ["BTC", "ETH"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn changing_the_period_reorders_at_once(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "SOL"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", -0.7), ("SOL", 1.3)]), cx)
        });
        window.render_frame(cx);
        choose_sort(window, "Change, gainers first", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["SOL", "BTC"]);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // The new period's figures arrive with the change of period.
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", 3.0), ("SOL", 1.3)]), cx)
        });
        choose_period(window, "7 days", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["BTC", "SOL"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_switch_shows_funding_with_the_footer_and_no_trading_control(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // Not shown until chosen.
        assert!(window.try_find("funding").is_none());
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_positions(
                vec![position("BTC", "", 50_000), position("NVDA", "xyz", 15_000)],
                cx,
            );
            view.set_rates(&[("BTC", "0.00001"), ("xyz:NVDA", "-0.00002")], cx);
        });
        window.within("sections").click(2usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), Some(Section::Funding));
        assert!(window.try_find("funding").is_some());
        assert!(window.try_find("funding-rows").is_some());
        // The account footer is under this section too.
        assert!(window.try_find("account-footer").is_some());
        for forbidden in [
            "Trade", "Buy", "Sell", "Close", "Cancel", "Deposit", "Withdraw", "Transfer",
        ] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "a `{forbidden}` control is in a read-only section"
            );
        }
        // And back to Portfolio.
        window.within("sections").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), Some(Section::Portfolio));
        assert!(window.try_find("funding").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn funding_with_no_positions_says_so_and_is_not_offered_before_connecting(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), None);
        assert!(window.find_all_by_label("Funding").is_empty());
    })
    .unwrap();

    let (window, desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| view.set_positions(Vec::new(), cx));
        window.within("sections").click(2usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("funding").is_some());
        // No rows to scroll: the section says there are no positions.
        assert!(window.try_find("funding-rows").is_none());
    })
    .unwrap();
}

// ---- Settings -------------------------------------------------------------

use galata_desk_core::network::Network;

/// Click the "Use testnet" switch.
fn flip_testnet(window: &mut gpui_kit::Window, cx: &mut gpui_kit::App) {
    window.click("testnet", cx);
}

/// A valid key, and the address of its wallet: a standard test vector.
const TEST_KEY: &str = "0x0000000000000000000000000000000000000000000000000000000000000001";
const TEST_KEY_ADDRESS: &str = "0x7e5f4552091a69125d5dfcb7b8c2659029395bdf";

#[gpui_kit::test]
fn settings_opens_over_a_section_and_returns_to_it_unchanged(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.within("colour").click(2usize, cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-page").is_some());
        // The section is not shown while settings is, and is still there.
        assert!(window.try_find("tiles").is_none());
        assert_eq!(desk.read(cx).section(), Some(Section::Markets));
        window.click("settings-done", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-page").is_none());
        assert_eq!(desk.read(cx).section(), Some(Section::Markets));
        // The same Markets view, with its selection kept.
        assert_eq!(desk.read(cx).markets_view().unwrap(), markets);
        assert!(markets.read(cx).selections(cx).ends_with("colour=Funding"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn settings_is_reachable_before_connecting(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-page").is_some());
        window.click("settings-done", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("settings-page").is_none());
        assert_eq!(desk.read(cx).section(), None);
        assert!(window.try_find("connect").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_testnet_switch_changes_the_network_marks_it_and_is_remembered(cx: &mut TestAppContext) {
    let (window, desk, dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Core::get(cx).network, Network::Mainnet);
        assert!(window.try_find("testnet-badge").is_none());
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let before = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let before = desk.read(cx).portfolio_view().unwrap();
            flip_testnet(window, cx);
            before
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Core::get(cx).network, Network::Testnet);
        assert!(window.try_find("testnet-badge").is_some());
        // The sections were built afresh for the new network, and settings
        // is still open over them.
        assert_ne!(desk.read(cx).portfolio_view().unwrap(), before);
        assert!(window.try_find("settings-page").is_some());
        // The addresses in tests are development overrides, which win.
        assert!(Core::get(cx).overridden());
        assert_eq!(Core::get(cx).info_url(), "http://127.0.0.1:9/info");
    })
    .unwrap();
    // Remembered at once.
    assert_eq!(Store::at(dir.path()).settings().network, Network::Testnet);

    // And back: the mark goes.
    cx.update_window(window.into(), |_, window, cx| flip_testnet(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Core::get(cx).network, Network::Mainnet);
        assert!(window.try_find("testnet-badge").is_none());
    })
    .unwrap();
    assert_eq!(Store::at(dir.path()).settings().network, Network::Mainnet);
}

#[gpui_kit::test]
fn the_app_starts_on_the_remembered_network(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    Store::at(dir.path())
        .save_settings(&galata_desk_core::store::Settings {
            network: Network::Testnet,
            ..Default::default()
        })
        .unwrap();
    assert_eq!(Store::at(dir.path()).settings().network, Network::Testnet);
    // `open` reads the stored network as `main` does; a fresh directory is mainnet.
    let (window, _desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Core::get(cx).network, Network::Mainnet);
        assert!(window.try_find("testnet-badge").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_api_key_is_saved_shown_only_as_an_address_and_removed(cx: &mut TestAppContext) {
    let (window, desk, dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let settings = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let settings = desk.read(cx).settings_view().unwrap();
            assert_eq!(settings.read(cx).saved_address(), None);
            settings.update(cx, |view, cx| view.set_key_text(TEST_KEY, window, cx));
            window.render_frame(cx);
            window.click("save-key", cx);
            settings
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let view = settings.read(cx);
        assert_eq!(view.saved_address().as_deref(), Some(TEST_KEY_ADDRESS));
        // The field is emptied, and the entry is in the secret store.
        assert_eq!(view.key_text(cx), "");
        assert_eq!(
            view.message(),
            Some(("Saved to the keychain.".into(), false))
        );
        let stored = Core::get(cx).secrets.get("api-key-mainnet").unwrap();
        assert_eq!(stored.as_deref(), Some(&TEST_KEY[2..]));
        // Nothing on the page reads as the key.
        assert!(window.find_all_by_label(TEST_KEY).is_empty());
        assert!(window.find_all_by_label(&TEST_KEY[2..]).is_empty());
        assert!(window.try_find("save-key").is_none());
    })
    .unwrap();

    // No file in the app's directory holds the key.
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default();
        assert!(!text.contains(&TEST_KEY[2..]), "a file holds the key");
    }

    // Each network has its own key: testnet has none.
    cx.update_window(window.into(), |_, window, cx| flip_testnet(window, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(settings.read(cx).saved_address(), None);
        assert!(window.try_find("save-key").is_some());
        flip_testnet(window, cx);
    })
    .unwrap();
    cx.run_until_parked();

    // Back on mainnet the key is there, and Remove deletes it.
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            settings.read(cx).saved_address().as_deref(),
            Some(TEST_KEY_ADDRESS)
        );
        window.click("remove-key", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(settings.read(cx).saved_address(), None);
        assert_eq!(Core::get(cx).secrets.get("api-key-mainnet").unwrap(), None);
        assert!(window.try_find("save-key").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn text_that_is_not_a_key_is_refused_and_nothing_is_stored(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, false);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let settings = desk.read(cx).settings_view().unwrap();
        for bad in ["", "hello", TEST_KEY_ADDRESS] {
            settings.update(cx, |view, cx| view.set_key_text(bad, window, cx));
            window.render_frame(cx);
            window.click("save-key", cx);
            window.render_frame(cx);
            let (message, problem) = settings.read(cx).message().unwrap();
            assert!(problem, "{bad:?}");
            assert!(message.contains("64 hexadecimal characters"), "{message}");
            assert_eq!(settings.read(cx).saved_address(), None);
        }
        assert_eq!(Core::get(cx).secrets.get("api-key-mainnet").unwrap(), None);
    })
    .unwrap();
}

#[gpui_kit::test]
fn disconnecting_keeps_saved_api_keys(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        Core::get(cx)
            .secrets
            .set("api-key-mainnet", &TEST_KEY[2..])
            .unwrap();
        window.click("disconnect", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), None);
        assert!(
            Core::get(cx)
                .secrets
                .get("api-key-mainnet")
                .unwrap()
                .is_some()
        );
    })
    .unwrap();
}

// ---- The chart: loading, failed, and the account's fills -------------------

use galata_desk_core::market::candles::Candle;
use galata_desk_core::market::feed::Chart;
use galata_desk_core::market::fills::{Fill, FillSide, Fills};
use galata_desk_core::market::{Interval, MarketPeriod};

const CANDLE_T0: u64 = 1_794_900_000_000;

/// A snapshot with a chart of `count` 15-minute candles for `coin`.
fn charted(coin: &str, watched: &[&str], count: u64) -> MarketSnapshot {
    let mut snapshot = snapshot(watched, watched);
    snapshot.chart = Some(Chart {
        market: MarketId::new(coin),
        interval: Interval::M15,
        candles: (0..count)
            .map(|i| Candle {
                open_ms: CANDLE_T0 + i * Interval::M15.millis(),
                open: Decimal::from(100 + i),
                high: Decimal::from(102 + i),
                low: Decimal::from(99 + i),
                close: Decimal::from(101 + i),
            })
            .collect(),
    });
    snapshot
}

fn a_fill(coin: &str, side: FillSide, candle: u64) -> Fill {
    Fill {
        coin: coin.into(),
        side,
        price: Decimal::from(100),
        size: Decimal::ONE,
        time_ms: CANDLE_T0 + candle * Interval::M15.millis() + 1,
        realised: Decimal::ZERO,
        fee: Decimal::ZERO,
        id: candle,
        ..Fill::blank()
    }
}

#[gpui_kit::test]
fn the_chart_area_shows_loading_then_failed_then_the_chart(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(MarketPeriod::default().interval(), Interval::M15);
        // No chart yet and no failure: loading, and it says what.
        markets.update(cx, |view, cx| {
            view.set_snapshot(snapshot(&["BTC", "ETH"], &["BTC", "ETH"]), cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart(cx).0, "loading");
        assert!(window.try_find("chart-loading").is_some());
        assert!(window.try_find("chart-failed").is_none());

        // With reduced motion asked for, the placeholders hold still and the
        // text still says what is loading.
        cx.set_reduce_motion(true);
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart(cx).0, "loading");
        assert!(window.try_find("chart-loading-still").is_some());
        assert!(window.try_find("chart-loading").is_none());
        cx.set_reduce_motion(false);

        // The load failed: the chart area says so.
        let mut failed = snapshot(&["BTC", "ETH"], &["BTC", "ETH"]);
        failed.chart_failed = true;
        markets.update(cx, |view, cx| view.set_snapshot(failed, cx));
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart(cx).0, "failed");
        assert!(window.try_find("chart-failed").is_some());
        assert!(window.try_find("chart-loading").is_none());

        // A later attempt succeeds: the chart, and no message.
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart(cx).0, "chart");
        assert!(window.try_find("chart-failed").is_none());
        assert!(window.try_find("chart-loading").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_accounts_open_orders_are_levels_on_the_selected_markets_chart(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // The candles run from 99 to 141.
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        window.render_frame(cx);
        assert!(window.try_find(("order-level", 0usize)).is_none());

        let take_profit = OrderRow {
            kind: "Take Profit Market".into(),
            remaining: Decimal::ZERO,
            trigger_px: Some(160.into()),
            tpsl: Some(galata_desk_core::trade::sign::Tpsl::Tp),
            ..a_limit(2, "BTC", Side::Sell, "0", "0")
        };
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_orders(
                vec![
                    a_limit(1, "BTC", Side::Buy, "0.5", "110"),
                    take_profit,
                    // Another market's order is not this chart's.
                    a_limit(3, "ETH", Side::Buy, "2", "120"),
                ],
                cx,
            )
        });
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);

        // Inside the range: a line and a tag at its price, which says what
        // the order is.
        assert!(window.try_find(("order-line", 0usize)).is_some());
        assert_eq!(
            text_at(window, ("order-level", 0usize)).as_deref(),
            Some("Limit \u{b7} Buy 0.5 BTC at 110")
        );
        // Above it, the take-profit's trigger: a tag at the top edge, and
        // no line.
        assert!(window.try_find(("order-line", 1usize)).is_none());
        assert!(window.try_find(("order-level", 1usize)).is_none());
        assert_eq!(
            text_at(window, "order-above").as_deref(),
            Some("Take Profit Market \u{b7} Sell the position at 160, above the chart")
        );
        assert!(window.try_find(("order-level", 2usize)).is_none());
        assert!(window.try_find("order-below").is_none());

        // Several beyond an edge share one tag, which counts them and names
        // the nearest.
        portfolio.update(cx, |view, cx| {
            view.set_orders(
                vec![
                    a_limit(1, "BTC", Side::Sell, "0.5", "170"),
                    a_limit(2, "BTC", Side::Sell, "0.5", "150"),
                    a_limit(3, "BTC", Side::Buy, "0.5", "90"),
                ],
                cx,
            )
        });
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(
            text_at(window, "order-above").as_deref(),
            Some("2 orders above the chart. The nearest: Limit \u{b7} Sell 0.5 BTC at 150")
        );
        assert_eq!(
            text_at(window, "order-below").as_deref(),
            Some("Limit \u{b7} Buy 0.5 BTC at 90, below the chart")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_accounts_fills_are_marked_on_the_selected_markets_chart(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        window.render_frame(cx);
        // Until fills arrive the chart is drawn without marks.
        assert_eq!(markets.read(cx).drawn_chart(cx), ("chart", 0, 0, None));

        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_fills(
                Fills::new(vec![
                    // Two buys in candle 5 are one mark; a sell there is another.
                    a_fill("BTC", FillSide::Bought, 5),
                    a_fill("BTC", FillSide::Bought, 5),
                    a_fill("BTC", FillSide::Sold, 5),
                    a_fill("BTC", FillSide::Sold, 20),
                    // Another market's fills are not this chart's.
                    a_fill("ETH", FillSide::Bought, 7),
                ]),
                cx,
            )
        });
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart(cx), ("chart", 1, 2, None));
        assert!(window.try_find(("fill-b", 5usize)).is_some());
        assert!(window.try_find(("fill-s", 5usize)).is_some());
        assert!(window.try_find(("fill-s", 20usize)).is_some());
        assert!(window.try_find(("fill-b", 7usize)).is_none());
        // Few fills: the venue did not cut them short, so nothing is said.
        assert!(window.try_find("trades-since").is_none());

        // The other market's chart shows its own fill and none of BTC's.
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("ETH", &["BTC", "ETH"], 40), cx)
        });
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));
        assert_eq!(markets.read(cx).drawn_chart(cx), ("chart", 1, 0, None));
        assert!(window.try_find(("fill-b", 7usize)).is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_chart_says_from_when_trades_are_shown_only_when_the_venue_cut_them_short(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC"], 40), cx)
        });
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        // As many fills as the venue gives at once, the earliest in candle 30.
        let mut rows = vec![a_fill("ETH", FillSide::Bought, 30); 1999];
        rows.push(a_fill("BTC", FillSide::Sold, 35));
        portfolio.update(cx, |view, cx| view.set_fills(Fills::new(rows), cx));
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        let (state, bought, sold, since) = markets.read(cx).drawn_chart(cx);
        assert_eq!((state, bought, sold), ("chart", 0, 1));
        assert_eq!(since, Some(CANDLE_T0 + 30 * Interval::M15.millis() + 1));
        assert!(window.try_find("trades-since").is_some());
    })
    .unwrap();
}

// ---- Tiles: the range bar, position and PNL --------------------------------

/// What the figure with this id reads, in the last frame: its accessible
/// label, which is the text it shows. `None` while a placeholder stands in.
fn text_at(window: &gpui_kit::Window, id: impl Into<gpui_kit::ElementId>) -> Option<String> {
    window
        .try_find(id)
        .and_then(|e| e.label().map(str::to_string))
}

/// A tile with a live price and a period's statistics: a range of 100 to
/// 110, averaging 105 with a spread of 2.
fn priced(coin: &str, last: &str, funding: &str) -> Tile {
    use galata_desk_core::market::candles::Stats;
    use galata_desk_core::market::feed::Live;
    let px: Decimal = last.parse().unwrap();
    Tile {
        market: MarketId::new(coin),
        live: Some(Live {
            last: px,
            mark: px,
            oracle: px,
            premium: None,
            funding: funding.parse().unwrap(),
            volume_usd: Decimal::ZERO,
            open_interest_usd: Decimal::ZERO,
        }),
        stats: Some(Stats {
            start: Decimal::from(100),
            change: -0.027,
            low: Decimal::from(100),
            high: Decimal::from(110),
            marker: 0.0,
            volatility: Some(0.012),
            mean: Some(105.0),
            sd: Some(2.0),
        }),
        max_leverage: Some(10),
        unavailable: false,
    }
}

fn priced_snapshot(coins: &[&str]) -> MarketSnapshot {
    let mut snapshot = snapshot(coins, coins);
    snapshot.tiles = coins.iter().map(|c| priced(c, "100", "0.000008")).collect();
    snapshot
}

const DAY_MS: u64 = 24 * 60 * 60 * 1000;

/// A fill `days` ago that realised `realised` and paid `fee`.
fn pnl_fill(coin: &str, days: u64, realised: &str, fee: &str, id: u64) -> Fill {
    Fill {
        coin: coin.into(),
        side: FillSide::Sold,
        price: Decimal::ONE,
        size: Decimal::ONE,
        time_ms: now_ms() - days * DAY_MS,
        realised: realised.parse().unwrap(),
        fee: fee.parse().unwrap(),
        id,
        ..Fill::blank()
    }
}

fn walked(rows: Vec<Fill>) -> Fills {
    Fills {
        rows,
        capped: false,
        walked: true,
    }
}

fn give_account(
    desk: &Entity<Desk>,
    positions: Option<Vec<PositionRow>>,
    fills: Option<Fills>,
    cx: &mut gpui_kit::App,
) {
    let portfolio = desk.read(cx).portfolio_view().unwrap();
    portfolio.update(cx, |view, cx| {
        if let Some(positions) = positions {
            view.set_positions(positions, cx);
        }
        if let Some(fills) = fills {
            view.set_fills(fills, cx);
        }
    });
}

#[gpui_kit::test]
fn a_tile_shows_the_position_and_pnl_for_long_short_closed_and_untraded_markets(
    cx: &mut TestAppContext,
) {
    let coins = ["BTC", "xyz:NVDA", "DOGE", "xyz:GOLD"];
    let (window, desk, _dir) = open_with(cx, Some(&coins));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&coins), cx)
        });
        give_account(
            &desk,
            Some(vec![
                PositionRow {
                    asset: None,
                    pnl: Decimal::from(-2698),
                    ..position("BTC", "", 412_300)
                },
                short("NVDA", "xyz", 18_700),
            ]),
            Some(walked(vec![
                pnl_fill("BTC", 3, "700", "25", 1),
                pnl_fill("BTC", 10, "500", "15", 2),
                // DOGE was closed 40 days ago; nothing since.
                pnl_fill("DOGE", 40, "-2180", "10", 3),
            ])),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);

        // Long BTC: the value with a plus, and 1,200 realised less 40 in
        // fees, plus the open position's -2,698.
        assert_eq!(
            text_at(window, ("tile-pos", 0usize)).as_deref(),
            Some("+$412k")
        );
        assert_eq!(
            text_at(window, ("tile-30d", 0usize)).as_deref(),
            Some("-$1,538")
        );
        // Short NVDA: the value with a minus; no trades, no unrealised PNL.
        assert_eq!(
            text_at(window, ("tile-pos", 1usize)).as_deref(),
            Some("-$19k")
        );
        assert_eq!(text_at(window, ("tile-30d", 1usize)).as_deref(), Some("$0"));
        // DOGE: no position and nothing in 30 days, but the year still counts.
        assert_eq!(
            text_at(window, ("tile-pos", 2usize)).as_deref(),
            Some("\u{2014}")
        );
        assert_eq!(
            text_at(window, ("tile-30d", 2usize)).as_deref(),
            Some("\u{2014}")
        );
        assert_eq!(
            text_at(window, ("tile-all", 2usize)).as_deref(),
            Some("-$2,190")
        );
        // GOLD was never traded: dashes throughout.
        for id in ["tile-pos", "tile-30d", "tile-ytd", "tile-all"] {
            assert_eq!(
                text_at(window, (id, 3usize)).as_deref(),
                Some("\u{2014}"),
                "{id}"
            );
        }
        // Volatility is beside the price, and every tile is the same height.
        assert_eq!(
            text_at(window, ("tile-vol", 0usize)).as_deref(),
            Some("1.2%")
        );
        let heights: Vec<_> = (0..4usize)
            .map(|ix| window.find(("tile", ix)).bounds().size.height)
            .collect();
        assert!(heights.windows(2).all(|w| w[0] == w[1]), "{heights:?}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn sorted_by_funding_the_tile_shows_the_rate_beside_the_price(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&["BTC"]), cx)
        });
        window.render_frame(cx);
        choose_sort(
            window,
            galata_desk_core::market::sort::SortBy::Funding.words(),
            cx,
        );
        assert_eq!(
            text_at(window, ("tile-vol", 0usize)).as_deref(),
            Some("+0.0008%")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_bar_shows_how_far_the_price_is_from_its_average(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let mut snapshot = priced_snapshot(&["BTC", "ETH"]);
        // ETH at its average; BTC at the low, 2.5 deviations below.
        snapshot.tiles[1] = priced("ETH", "105", "0");
        markets.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        window.render_frame(cx);
        assert_eq!(
            text_at(window, ("tile-sigma", 0usize)).as_deref(),
            Some("-2.5\u{3c3}")
        );
        assert_eq!(
            text_at(window, ("tile-sigma", 1usize)).as_deref(),
            Some("0.0\u{3c3}")
        );
        // A period with too few closes has a bar but no figure.
        let mut snapshot = priced_snapshot(&["BTC", "ETH"]);
        if let Some(stats) = snapshot.tiles[0].stats.as_mut() {
            stats.mean = None;
            stats.sd = None;
        }
        markets.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        window.render_frame(cx);
        assert!(window.try_find(("tile-bar", 0usize)).is_some());
        assert!(window.try_find(("tile-sigma", 0usize)).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn position_and_pnl_wait_for_the_account_and_never_show_zeros(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // History still loading: no bar yet.
        markets.update(cx, |view, cx| {
            view.set_snapshot(snapshot(&["BTC"], &["BTC"]), cx)
        });
        window.render_frame(cx);
        assert!(window.try_find(("tile-bar", 0usize)).is_none());

        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&["BTC"]), cx)
        });
        window.render_frame(cx);
        assert!(window.try_find(("tile-bar", 0usize)).is_some());
        // Nothing from the account yet: placeholders, not zeros or dashes.
        for id in ["tile-pos", "tile-30d", "tile-ytd", "tile-all"] {
            assert_eq!(text_at(window, (id, 0usize)), None, "{id}");
        }

        // Positions, and only the latest fills: the PNL still waits.
        give_account(
            &desk,
            Some(vec![position("BTC", "", 5_000)]),
            Some(Fills {
                walked: false,
                ..walked(vec![pnl_fill("BTC", 1, "10", "0", 1)])
            }),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(
            text_at(window, ("tile-pos", 0usize)).as_deref(),
            Some("+$5.0k")
        );
        assert_eq!(text_at(window, ("tile-30d", 0usize)), None);

        // The walk is done: the PNL shows.
        give_account(
            &desk,
            None,
            Some(walked(vec![pnl_fill("BTC", 1, "10", "0", 1)])),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(
            text_at(window, ("tile-30d", 0usize)).as_deref(),
            Some("+$10")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_legend_says_when_pnl_is_partial(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&["BTC"]), cx)
        });
        give_account(
            &desk,
            Some(vec![]),
            Some(walked(vec![pnl_fill("BTC", 400, "100", "0", 1)])),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        // The legend keeps to the colour scale and the held-position dot.
        assert!(window.try_find("legend-bar").is_none());
        assert!(window.try_find("legend-strip").is_none());
        assert!(window.try_find("pnl-since").is_none());
        assert_eq!(
            text_at(window, ("tile-all", 0usize)).as_deref(),
            Some("+$100")
        );

        // The venue held older fills back: all-time is marked, and the
        // section says from when trades count.
        give_account(
            &desk,
            None,
            Some(Fills {
                capped: true,
                ..walked(vec![pnl_fill("BTC", 400, "100", "0", 1)])
            }),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert!(window.try_find("pnl-since").is_some());
        assert_eq!(
            text_at(window, ("tile-all", 0usize)).as_deref(),
            Some("+$100*")
        );
        // 30 days lies within the fills held: not marked.
        assert_eq!(
            text_at(window, ("tile-30d", 0usize)).as_deref(),
            Some("\u{2014}")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_screen_reader_hears_the_tiles_figures(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["HYPE"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&["HYPE"]), cx)
        });
        give_account(
            &desk,
            Some(vec![PositionRow {
                asset: None,
                pnl: Decimal::from(-1538),
                ..position("HYPE", "", 57_007)
            }]),
            Some(walked(vec![])),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(
            window.find(("tile", 0usize)).label(),
            Some(
                "HYPE, Crypto, last 100, -2.70%, -2.5 standard deviations from the period average, \
                 position +$57k, 30-day PNL -$1,538, year-to-date PNL -$1,538, all-time PNL -$1,538"
            )
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn at_the_smallest_window_a_whole_row_of_tiles_shows_below_the_panel(cx: &mut TestAppContext) {
    let smallest = size(px(crate::MIN_WINDOW_WIDTH), px(crate::MIN_WINDOW_HEIGHT));
    let (window, desk, dir) = open_sized(cx, true, smallest);
    let coins = ["BTC", "ETH", "SOL", "HYPE", "xyz:NVDA", "xyz:GOLD", "DOGE"];
    Store::at(dir.path())
        .save_watchlist(&watchlist(&coins))
        .unwrap();
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&coins), cx)
        });
        give_account(
            &desk,
            Some(vec![position("BTC", "", 1_000)]),
            Some(Fills {
                capped: true,
                ..walked(vec![pnl_fill("BTC", 1, "1", "0", 1)])
            }),
            cx,
        );
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        let panel = window.find("market-panel").bounds();
        let tiles = window.find("tiles").bounds();
        let first = window.find(("tile", 0usize)).bounds();
        assert!(panel.bottom() <= tiles.top(), "{panel:?} {tiles:?}");
        assert!(
            first.bottom() <= tiles.bottom(),
            "the first row is cut off: tile {first:?}, room {tiles:?}"
        );
    })
    .unwrap();
}

// ---- Order actions -------------------------------------------------------

use std::rc::Rc;

use galata_desk_core::model::OrderRow;
use galata_desk_core::trade::assets::Asset;
use galata_desk_core::trade::{Answer, Outcome, Target, Traded, Unavailable};
use galata_desk_core::venue::Side;

use crate::portfolio::{Portfolio, Staged};

/// The market the test orders rest in: a builder-dex stock, sizes to two places.
const NVDA: Asset = Asset {
    number: 110_000,
    sz_decimals: 2,
    spot: false,
};

/// A resting limit order.
fn a_limit(oid: u64, market: &str, side: Side, size: &str, price: &str) -> OrderRow {
    OrderRow {
        placed_ms: 1_794_900_000_000 + oid,
        market: market.into(),
        dex: String::new(),
        kind: "Limit".into(),
        side,
        remaining: size.parse().unwrap(),
        original: size.parse().unwrap(),
        price: price.parse().unwrap(),
        reduce_only: false,
        trigger: None,
        oid,
        tif: Some("Gtc".into()),
        amendable: true,
        asset: Some(NVDA),
        trigger_px: None,
        tpsl: None,
    }
}

/// A stop order: it can be cancelled and not amended.
fn a_stop(oid: u64) -> OrderRow {
    OrderRow {
        kind: "Stop Market".into(),
        reduce_only: true,
        trigger: Some("Price below 140".into()),
        trigger_px: Some(140.into()),
        tif: None,
        amendable: false,
        ..a_limit(oid, "NVDA", Side::Sell, "12", "139")
    }
}

fn three_orders() -> Vec<OrderRow> {
    vec![
        a_limit(1, "NVDA", Side::Buy, "12", "154.20"),
        a_limit(2, "HYPE", Side::Sell, "40", "41.85"),
        a_limit(3, "BTC", Side::Buy, "0.05", "91400"),
    ]
}

/// Open the Open orders tab with these orders and this stand-in for the gate.
fn orders_tab(
    cx: &mut TestAppContext,
    staged: &Rc<Staged>,
    orders: Vec<OrderRow>,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<Portfolio>,
    tempfile::TempDir,
) {
    let (window, desk, dir) = open(cx, true);
    let view = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let view = desk.read(cx).portfolio_view().unwrap();
            view.update(cx, |view, cx| {
                view.use_trading(staged.clone());
                view.set_orders(orders, cx);
            });
            window.within("holdings").click(2usize, cx);
            view
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    (window, desk, view, dir)
}

/// Press a row's button, as a click on it does, and let the effect land.
fn press_order(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    view: &Entity<Portfolio>,
    id: &str,
    oid: u64,
) {
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.press_order(id, oid, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

/// Click something in the window and let the effect land.
fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) {
    cx.update_window(window.into(), |_, window, cx| window.click(id, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

#[gpui_kit::test]
fn a_limit_order_offers_amend_and_cancel_and_a_stop_only_cancel(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let orders = vec![a_limit(1, "NVDA", Side::Buy, "12", "154.20"), a_stop(2)];
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, orders);
    cx.update_window(window.into(), |_, window, cx| {
        let controls = view.read(cx).order_controls();
        assert_eq!(
            controls,
            [
                (1, "Amend order".to_string(), false, false),
                (1, "Cancel order".to_string(), false, false),
                (2, "Cancel order".to_string(), false, false),
            ]
        );
        assert!(view.read(cx).can_cancel_all());
        assert_eq!(view.read(cx).orders_unavailable(), None);
        assert!(window.try_find("orders-notice").is_none());
        // The buttons are in the window under those names, and Cancel all
        // has its own.
        assert_eq!(window.find_all_by_label("Amend order").len(), 1);
        assert_eq!(window.find_all_by_label("Cancel order").len(), 2);
        assert_eq!(window.find_all_by_label("Cancel all open orders").len(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_order_in_a_market_the_venue_does_not_list_cannot_be_acted_on(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let unknown = OrderRow {
        asset: None,
        ..a_limit(1, "GONE", Side::Buy, "12", "154.20")
    };
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, vec![unknown]);
    cx.update_window(window.into(), |_, _, cx| {
        let controls = view.read(cx).order_controls();
        assert!(controls.iter().all(|(_, _, disabled, _)| *disabled));
        assert!(controls[1].1.contains("not loaded yet"), "{controls:?}");
        assert!(!view.read(cx).can_cancel_all());
    })
    .unwrap();
    press_order(cx, window, &view, "cancel-order", 1);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
}

#[gpui_kit::test]
fn each_reason_orders_cannot_be_acted_on_is_said_and_disables_the_controls(
    cx: &mut TestAppContext,
) {
    for (unavailable, heading, settings) in [
        (
            Unavailable::NoKey(Network::Testnet),
            "An API key is needed to amend or cancel orders",
            true,
        ),
        (
            Unavailable::KeyNotApproved,
            "The saved key cannot act for this account",
            true,
        ),
        (
            Unavailable::MainnetOff,
            "Trading on mainnet is switched off",
            true,
        ),
        (Unavailable::Overridden, "Order actions are off", false),
    ] {
        let staged = Rc::new(Staged::new(Err(unavailable)));
        let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());
        cx.update_window(window.into(), |_, window, cx| {
            assert_eq!(view.read(cx).orders_unavailable().as_deref(), Some(heading));
            assert!(window.try_find("orders-notice").is_some(), "{heading}");
            assert_eq!(
                window.try_find("open-settings").is_some(),
                settings,
                "{heading}"
            );
            // The orders are listed as usual; only the controls are off.
            let controls = view.read(cx).order_controls();
            assert_eq!(controls.len(), 6);
            assert!(controls.iter().all(|(_, _, disabled, _)| *disabled));
            assert!(!view.read(cx).can_cancel_all());
        })
        .unwrap();
        // Nothing opens, by a row's button or by Cancel all.
        press_order(cx, window, &view, "cancel-order", 1);
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.press_cancel_all(window, cx));
            assert_eq!(view.read(cx).open_dialog(), None);
        })
        .unwrap();
        assert!(staged.cancels.borrow().is_empty());
    }
}

#[gpui_kit::test]
fn with_a_development_override_the_real_gate_keeps_the_tab_read_only(cx: &mut TestAppContext) {
    // These tests run with the venue's addresses overridden, and a key saved
    // changes nothing about that.
    let (window, desk, _dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let core = Core::get(cx);
        core.secrets
            .set(
                galata_desk_core::secrets::api_key_name(core.network),
                TEST_KEY,
            )
            .unwrap();
        let view = desk.read(cx).portfolio_view().unwrap();
        view.update(cx, |view, cx| {
            view.reconsider_trading(cx);
            view.set_orders(three_orders(), cx);
        });
        assert_eq!(
            view.read(cx).orders_unavailable().as_deref(),
            Some("Order actions are off")
        );
        assert!(
            view.read(cx)
                .order_controls()
                .iter()
                .all(|(_, _, disabled, _)| *disabled)
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_notice_leads_to_settings(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Err(Unavailable::NoKey(Network::Mainnet))));
    let (window, _desk, _view, _dir) = orders_tab(cx, &staged, three_orders());
    click(cx, window, "open-settings");
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find("settings-page").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancelling_asks_first_and_sends_only_on_confirmation(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());

    press_order(cx, window, &view, "cancel-order", 1);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("cancel"));
        assert!(window.try_find("cancel-dialog").is_some());
    })
    .unwrap();
    // Choosing the button sent nothing.
    assert!(staged.cancels.borrow().is_empty());

    // Backing out leaves the order alone.
    click(cx, window, "dialog-keep");
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
        assert!(window.try_find("cancel-dialog").is_none());
        assert_eq!(view.read(cx).orders_reported(), None);
    })
    .unwrap();
    assert!(staged.cancels.borrow().is_empty());

    // So does Escape.
    press_order(cx, window, &view, "cancel-order", 1);
    cx.update_window(window.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert!(staged.cancels.borrow().is_empty());

    // Confirming sends that order, and only it.
    press_order(cx, window, &view, "cancel-order", 1);
    click(cx, window, "dialog-confirm");
    assert_eq!(
        *staged.cancels.borrow(),
        [vec![Target {
            asset: 110_000,
            oid: 1
        }]]
    );
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.open_dialog(), None);
        assert_eq!(
            view.orders_reported(),
            Some(("Cancelled: Buy 12 NVDA at 154.20.".into(), None, false))
        );
        // The orders were asked for again at once.
        assert_eq!(view.orders_refreshes(), 1);
        assert!(view.orders_pending().is_empty());
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancel_all_lists_what_it_will_cancel_and_reports_each_order(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged.answers.borrow_mut().push_back(Ok(Answer::Each(vec![
        Outcome::DONE,
        Outcome::Refused("Order was never placed, already canceled, or filled.".into()),
        Outcome::DONE,
    ])));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());

    click(cx, window, "cancel-all");
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("cancel-all"));
        assert!(window.try_find("cancel-all-dialog").is_some());
    })
    .unwrap();
    assert!(staged.cancels.borrow().is_empty());

    click(cx, window, "dialog-confirm");
    // One request, naming every listed order.
    assert_eq!(
        *staged.cancels.borrow(),
        [vec![
            Target {
                asset: 110_000,
                oid: 1
            },
            Target {
                asset: 110_000,
                oid: 2
            },
            Target {
                asset: 110_000,
                oid: 3
            },
        ]]
    );
    cx.update_window(window.into(), |_, _, cx| {
        let (text, more, problem) = view.read(cx).orders_reported().unwrap();
        assert_eq!(text, "Cancelled 2 of 3 orders.");
        assert_eq!(
            more.as_deref(),
            Some(
                "Not cancelled: Sell 40 HYPE at 41.85 (Order was never placed, already canceled, or filled.)"
            )
        );
        assert!(problem);
        assert_eq!(view.read(cx).orders_refreshes(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancel_all_cannot_be_used_with_no_orders(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, Vec::new());
    click(cx, window, "cancel-all");
    cx.update_window(window.into(), |_, _, cx| {
        assert!(!view.read(cx).can_cancel_all());
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert!(staged.cancels.borrow().is_empty());
}

#[gpui_kit::test]
fn the_amend_form_checks_values_and_sends_them_as_shown(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());

    press_order(cx, window, &view, "amend-order", 1);
    let state = |cx: &mut TestAppContext, price: &str, size: &str| {
        cx.update_window(window.into(), |_, window, cx| {
            view.update(cx, |view, cx| view.type_amend(price, size, window, cx));
            window.render_frame(cx);
            view.read(cx).amend_state(cx).unwrap()
        })
        .unwrap()
    };
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("amend"));
        assert!(window.try_find("amend-dialog").is_some());
        // It starts from the order's own price and size, which is no change.
        assert_eq!(
            view.read(cx).amend_state(cx).unwrap(),
            ("Change the price or the size to amend.".into(), false)
        );
    })
    .unwrap();

    for (price, size, says) in [
        ("abc", "12", "Price must be a number above zero."),
        ("154.2", "0", "Size must be a number above zero."),
        (
            "154.201",
            "12",
            "Price is too precise: at most 5 significant figures and 4 decimal places here.",
        ),
        (
            "154.2",
            "12.001",
            "Size is too fine: at most 2 decimal places here.",
        ),
        (
            "154.2",
            "0.05",
            "The order would be worth less than the $10 minimum.",
        ),
    ] {
        assert_eq!(
            state(cx, price, size),
            (says.to_string(), false),
            "{price} x {size}"
        );
        // A form that is not valid cannot be sent.
        click(cx, window, "dialog-confirm");
        assert!(staged.amends.borrow().is_empty(), "{price} x {size}");
        cx.update_window(window.into(), |_, _, cx| {
            assert_eq!(view.read(cx).open_dialog(), Some("amend"));
        })
        .unwrap();
    }

    // Valid values: what the form shows is what is sent.
    assert_eq!(state(cx, "153.80", "15"), ("15 at 153.8".into(), true));
    click(cx, window, "dialog-confirm");
    {
        let amends = staged.amends.borrow();
        assert_eq!(amends.len(), 1);
        let (target, to, kept) = &amends[0];
        assert_eq!(
            *target,
            Target {
                asset: 110_000,
                oid: 1
            }
        );
        assert_eq!(
            (to.price_text.as_str(), to.size_text.as_str()),
            ("153.8", "15")
        );
        // Side, reduce-only and time in force are the order's own.
        assert_eq!(kept.side, Side::Buy);
        assert!(!kept.reduce_only);
        assert_eq!(kept.tif, "Gtc");
    }
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).orders_reported(),
            Some(("Amended: now Buy 15 NVDA at 153.8.".into(), None, false))
        );
        assert_eq!(view.read(cx).orders_refreshes(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_action_in_progress_shows_on_its_row_and_blocks_another(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged.hold.set(true);
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());
    press_order(cx, window, &view, "cancel-order", 2);
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.orders_pending(), [2]);
        let controls = view.order_controls();
        // Every control is off, and the order's own Cancel says what it is doing.
        assert!(controls.iter().all(|(_, _, disabled, _)| *disabled));
        let busy: Vec<_> = controls.iter().filter(|c| c.3).collect();
        assert_eq!(busy, [&(2, "Cancelling\u{2026}".to_string(), true, true)]);
        assert!(!view.can_cancel_all());
        assert_eq!(view.orders_reported(), None);
    })
    .unwrap();
    // No second action can be started while the first is with the venue.
    press_order(cx, window, &view, "cancel-order", 1);
    press_order(cx, window, &view, "amend-order", 1);
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.press_cancel_all(window, cx));
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert_eq!(staged.cancels.borrow().len(), 1);
}

#[gpui_kit::test]
fn a_refusal_a_silence_and_an_unsent_action_are_each_reported(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());
    let cancel_first = |cx: &mut TestAppContext, answer: Result<Answer, Unavailable>| {
        staged.answers.borrow_mut().push_back(answer);
        press_order(cx, window, &view, "cancel-order", 1);
        click(cx, window, "dialog-confirm");
        cx.update_window(window.into(), |_, _, cx| {
            view.read(cx).orders_reported().unwrap()
        })
        .unwrap()
    };

    // The venue's own words.
    let (text, _, problem) = cancel_first(
        cx,
        Ok(Answer::Each(vec![Outcome::Refused(
            "Order was never placed, already canceled, or filled.".into(),
        )])),
    );
    assert_eq!(
        text,
        "Hyperliquid refused the cancel: Order was never placed, already canceled, or filled."
    );
    assert!(problem);

    let (text, _, _) = cancel_first(cx, Ok(Answer::Refused("Too many requests".into())));
    assert_eq!(text, "Hyperliquid refused the cancel: Too many requests");

    // No answer: the outcome is not known, and the app says so.
    let (text, _, problem) = cancel_first(cx, Ok(Answer::Unknown("the request timed out".into())));
    assert!(text.starts_with("No answer from Hyperliquid (the request timed out)."));
    assert!(text.contains("may or may not have gone through"));
    assert!(problem);

    // The key could not be checked: nothing was sent, and it can be tried again.
    let (text, _, _) = cancel_first(
        cx,
        Err(Unavailable::KeyCheckFailed(
            "the venue could not be reached".into(),
        )),
    );
    assert!(text.starts_with("Nothing was sent: the key could not be checked"));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).orders_unavailable(), None);
        // The orders are reloaded after every one of these.
        assert_eq!(view.read(cx).orders_refreshes(), 4);
    })
    .unwrap();

    // The key turned out not to be this account's: from then on the tab says so.
    *staged.availability.borrow_mut() = Err(Unavailable::KeyNotApproved);
    let (text, _, _) = cancel_first(cx, Err(Unavailable::KeyNotApproved));
    assert_eq!(
        text,
        "Nothing was sent: the saved key cannot act for this account."
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            view.read(cx).orders_unavailable().as_deref(),
            Some("The saved key cannot act for this account")
        );
        assert!(window.try_find("orders-notice").is_some());
        assert!(
            view.read(cx)
                .order_controls()
                .iter()
                .all(|(_, _, disabled, _)| *disabled)
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_amend_that_fills_at_once_is_said(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged
        .answers
        .borrow_mut()
        .push_back(Ok(Answer::Each(vec![Outcome::Done {
            filled: Some(Traded {
                size: Some(12.into()),
                avg_price: Some(160.into()),
            }),
            resting: false,
        }])));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, three_orders());
    press_order(cx, window, &view, "amend-order", 1);
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| view.type_amend("160", "12", window, cx));
    })
    .unwrap();
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).orders_reported().unwrap().0,
            "Amended, and it filled at once: Buy 12 NVDA at 160."
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn trading_on_mainnet_is_off_until_switched_on_and_is_remembered(cx: &mut TestAppContext) {
    let (window, _desk, dir) = open(cx, true);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(!Core::get(cx).allow_mainnet_trading);
    })
    .unwrap();
    click(cx, window, "settings");
    cx.update_window(window.into(), |_, window, _| {
        // The page says what a saved key is used for.
        assert_eq!(
            window.find_all_by_label("Allow trading on mainnet").len(),
            1
        );
    })
    .unwrap();
    click(cx, window, "mainnet-trading");
    cx.update_window(window.into(), |_, _, cx| {
        assert!(Core::get(cx).allow_mainnet_trading);
        // The network was not touched.
        assert_eq!(Core::get(cx).network, Network::Mainnet);
    })
    .unwrap();
    let stored = Store::at(dir.path()).settings();
    assert!(stored.allow_mainnet_trading);
    assert_eq!(stored.network, Network::Mainnet);

    // Switching networks keeps the choice.
    cx.update_window(window.into(), |_, window, cx| flip_testnet(window, cx))
        .unwrap();
    cx.run_until_parked();
    let stored = Store::at(dir.path()).settings();
    assert!(stored.allow_mainnet_trading);
    assert_eq!(stored.network, Network::Testnet);

    // And off again.
    click(cx, window, "mainnet-trading");
    assert!(!Store::at(dir.path()).settings().allow_mainnet_trading);
}

#[test]
fn the_settings_page_says_what_a_saved_key_is_used_for() {
    use crate::settings::{KEY_USE, MAINNET_SWITCH_NOTE};
    for text in [KEY_USE, MAINNET_SWITCH_NOTE] {
        for action in [
            "place",
            "amend and cancel",
            "close",
            "positions",
            "take-profit and stop-loss",
        ] {
            assert!(text.contains(action), "{action:?} in {text:?}");
        }
    }
    assert!(KEY_USE.ends_with("each time you confirm one, and for nothing else."));
}

// ---- Funding: watched markets, the chosen row and its chart ---------------

use galata_desk_core::format as fmt;
use galata_desk_core::funding::history::{self as funding_history, Held, History, Resolution};

use crate::funding::{Funding, sample};

/// Click Funding in the section switch and return the view.
fn go_to_funding(
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
    cx: &mut TestAppContext,
) -> Entity<Funding> {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("sections").click(2usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        desk.read(cx)
            .funding_view()
            .expect("an account is connected")
    })
    .unwrap()
}

/// Give the portfolio positions and rates, as if the venue had reported them.
fn hold(
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
    positions: Vec<PositionRow>,
    rates: &[(&str, &str)],
    cx: &mut TestAppContext,
) {
    cx.update_window(window.into(), |_, _, cx| {
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |view, cx| {
            view.set_positions(positions, cx);
            view.set_rates(rates, cx);
        });
    })
    .unwrap();
}

fn short(market: &str, dex: &str, value: i64) -> PositionRow {
    PositionRow {
        asset: None,
        size: -Decimal::ONE,
        ..position(market, dex, value)
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

/// Ninety days of made-up funding for `market`, fully loaded.
fn charted_funding(market: &str) -> (galata_desk_core::funding::feed::ChartSnapshot, History) {
    let history = sample::history(now_ms(), 90 * 24, false);
    (
        sample::snapshot(market, Resolution::Day, history.clone(), false, false),
        history,
    )
}

const RATES: &[(&str, &str)] = &[
    ("BTC", "0.00001"),
    ("ETH", "0.00001"),
    ("SOL", "0.00001"),
    ("xyz:NVDA", "-0.00002"),
    ("DOGE", "-0.000005"),
    ("XRP", "0.000019"),
    ("xyz:TSLA", "0.000012"),
];

#[gpui_kit::test]
fn watched_markets_without_a_position_are_listed_under_the_positions(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["DOGE", "xyz:NVDA", "XRP", "xyz:TSLA"]));
    hold(
        window,
        &desk,
        vec![position("NVDA", "xyz", 15_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = funding.read(cx).drawn();
        // Positions by flow: BTC pays $0.50 an hour, NVDA earns $0.30.
        assert_eq!(drawn.positions, ["BTC", "xyz:NVDA"]);
        // Watched rows keep the watchlist's order, though XRP's rate is the
        // highest; NVDA is held, so it is a position and not listed twice.
        assert_eq!(drawn.watched, ["DOGE", "XRP", "xyz:TSLA"]);
        assert!(window.try_find("funding-rows").is_some());
        assert!(window.try_find("funding-watched").is_some());
        assert!(
            drawn
                .as_of
                .starts_with("2 positions \u{b7} 3 watched without a position"),
            "{}",
            drawn.as_of
        );
        // A watched row shows its rate and the rate over a year, and a dash
        // in every other figure column. XRP is the fourth row.
        let dash = "\u{2014}";
        assert_eq!(
            drawn.figures_by_row[3],
            format!("{dash}|{dash}|+0.0019%|+16.6%|{dash}|{dash}|{dash}")
        );
        // The summary counts positions only.
        assert_eq!(drawn.earn_pay, "1 earn \u{b7} 1 pay");
    })
    .unwrap();
}

#[gpui_kit::test]
fn watched_rows_ignore_how_markets_is_sorted_and_follow_a_change_to_the_list(
    cx: &mut TestAppContext,
) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC", "SOL", "ETH"]));
    hold(window, &desk, vec![position("BTC", "", 50_000)], RATES, cx);
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", -0.7), ("SOL", 0.2), ("ETH", 1.3)]), cx)
        });
        window.render_frame(cx);
        choose_sort(window, "Change, gainers first", cx);
        assert_eq!(markets.read(cx).drawn(cx), ["ETH", "SOL", "BTC"]);
    })
    .unwrap();
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        // Markets draws ETH before SOL; the stored order has SOL first.
        assert_eq!(funding.read(cx).drawn().watched, ["SOL", "ETH"]);
    })
    .unwrap();

    // The list changes while another section is shown.
    cx.update_window(window.into(), |_, window, cx| {
        window.within("sections").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    Store::at(dir.path())
        .save_watchlist(&watchlist(&["BTC", "SOL", "ETH", "XRP"]))
        .unwrap();
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(funding.read(cx).drawn().watched, ["SOL", "ETH", "XRP"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_no_open_position_the_watched_markets_are_still_listed(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["DOGE", "XRP"]));
    hold(window, &desk, Vec::new(), RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let view = funding.read(cx);
        assert!(window.try_find("funding-no-positions").is_some());
        assert!(window.try_find("funding-rows").is_none());
        assert_eq!(view.drawn().watched, ["DOGE", "XRP"]);
        // The first watched market is chosen and has a chart panel.
        assert_eq!(view.chosen_market().as_deref(), Some("DOGE"));
        assert!(window.try_find("funding-chart").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_nothing_held_and_nothing_watched_there_are_no_rows_and_no_chart(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(window, &desk, Vec::new(), RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let view = funding.read(cx);
        assert!(window.try_find("funding-no-positions").is_some());
        assert!(window.try_find("funding-watched").is_none());
        assert!(window.try_find("funding-chart").is_none());
        assert_eq!(view.chosen_market(), None);
        assert_eq!(view.drawn().chart, "none");
    })
    .unwrap();
}

#[gpui_kit::test]
fn before_markets_has_saved_a_list_funding_uses_the_list_it_would_start_with(
    cx: &mut TestAppContext,
) {
    // With positions that list is the positions themselves: nothing extra.
    let (window, desk, _dir) = open_with(cx, None);
    hold(window, &desk, vec![position("BTC", "", 50_000)], RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert!(funding.read(cx).drawn().watched.is_empty());
    })
    .unwrap();

    // With none it is the four majors.
    let (window, desk, _dir) = open_with(cx, None);
    hold(window, &desk, Vec::new(), RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            funding.read(cx).drawn().watched,
            ["BTC", "ETH", "SOL", "HYPE"]
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_watched_market_the_rates_do_not_include_shows_a_dash(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["GONE"]));
    hold(window, &desk, Vec::new(), RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let drawn = funding.read(cx).drawn();
        assert_eq!(drawn.watched, ["GONE"]);
        // Every figure column, the rate and yearly among them.
        assert_eq!(drawn.figures_by_row, [["\u{2014}"; 7].join("|")]);
        let _ = window;
    })
    .unwrap();
}

#[gpui_kit::test]
fn one_row_is_chosen_by_default_by_click_and_from_the_keyboard(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["DOGE", "XRP"]));
    hold(
        window,
        &desk,
        vec![position("ETH", "", 20_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        // The first row: the largest flow.
        assert_eq!(funding.read(cx).drawn().positions, ["BTC", "ETH"]);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("BTC"));

        // A click chooses: the second position, then the first watched row.
        window.click(("funding-row", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("ETH"));
        window.click(("funding-row", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("DOGE"));

        // Focus alone does not choose; Enter and Space do.
        funding.update(cx, |view, cx| view.focus_market("XRP", window, cx));
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("DOGE"));
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("XRP"));
        funding.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        window.press("space", cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("BTC"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_choice_follows_its_market_and_falls_to_the_first_row_when_it_goes(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(
        window,
        &desk,
        vec![position("ETH", "", 20_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.click(("funding-row", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("ETH"));
    })
    .unwrap();

    // ETH's rate jumps, so its row moves to the top: it is still the choice.
    hold(
        window,
        &desk,
        vec![position("ETH", "", 20_000), position("BTC", "", 50_000)],
        &[("BTC", "0.00001"), ("ETH", "0.0001")],
        cx,
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(funding.read(cx).drawn().positions, ["ETH", "BTC"]);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("ETH"));
    })
    .unwrap();

    // A round trip through another section keeps it.
    cx.update_window(window.into(), |_, window, cx| {
        window.within("sections").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("ETH"));
    })
    .unwrap();

    // The chosen position closes, and its market is not watched.
    hold(window, &desk, vec![position("BTC", "", 50_000)], RATES, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("BTC"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_chart_opens_on_d_and_keeps_its_resolution_across_rows(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(
        window,
        &desk,
        vec![position("ETH", "", 20_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(funding.read(cx).resolution(), Resolution::Day);
        assert!(
            funding
                .read(cx)
                .drawn()
                .chart_note
                .starts_with("Last 90 days"),
            "{}",
            funding.read(cx).drawn().chart_note
        );

        window.within("funding-resolution").click(1usize, cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).resolution(), Resolution::EightHours);
        assert!(
            funding
                .read(cx)
                .drawn()
                .chart_note
                .starts_with("Last 30 days")
        );

        // Another row: the resolution stays.
        window.click(("funding-row", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("ETH"));
        assert_eq!(funding.read(cx).resolution(), Resolution::EightHours);

        window.within("funding-resolution").click(0usize, cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).resolution(), Resolution::Hour);
        assert!(
            funding
                .read(cx)
                .drawn()
                .chart_note
                .starts_with("Last 7 days")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_a_position_the_chart_takes_its_side_and_says_what_todays_size_would_have_done(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(window, &desk, vec![short("NVDA", "xyz", 48_210)], RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    let (snapshot, history) = charted_funding("xyz:NVDA");
    cx.update_window(window.into(), |_, window, cx| {
        funding.update(cx, |view, cx| view.set_chart(snapshot, cx));
        window.render_frame(cx);
        let drawn = funding.read(cx).drawn();
        assert_eq!(drawn.chart, "chart");
        assert_eq!(drawn.chart_note, "Last 90 days \u{b7} you are short");
        assert_eq!(drawn.points, 90);
        assert!(window.try_find("funding-chart-plot").is_some());

        // The figures are the core's for the same history.
        let summary = history.window(Resolution::Day).summary().unwrap();
        let earned =
            funding_history::what_if(Held::Short, Decimal::from(48_210), summary.cumulative);
        assert!(earned > Decimal::ZERO);
        assert_eq!(
            drawn.figures,
            [
                format!("Average: {} per day", fmt::pct_signed(summary.average, 4)),
                format!(
                    "Yearly at that average: {}",
                    fmt::pct_signed(summary.yearly, 1)
                ),
                format!("Positive: {} of 90 days", summary.positive),
                format!("Cumulative: {}", fmt::pct_signed(summary.cumulative, 2)),
                format!(
                    "A short of today's size would have received: {}",
                    fmt::usd_signed(earned)
                ),
            ]
        );
        assert!(
            drawn.legend.contains("your short received funding"),
            "{}",
            drawn.legend
        );
    })
    .unwrap();

    // The same market held long: the same window is paid, not received.
    hold(
        window,
        &desk,
        vec![position("NVDA", "xyz", 50_000)],
        RATES,
        cx,
    );
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let drawn = funding.read(cx).drawn();
        assert_eq!(drawn.chart_note, "Last 90 days \u{b7} you are long");
        let last = drawn.figures.last().unwrap();
        assert!(
            last.starts_with("A long of today's size would have paid: -$"),
            "{last}"
        );
        assert!(drawn.legend.contains("your long received funding"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn without_a_position_the_chart_takes_no_side_and_shows_no_dollar_figure(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["XRP"]));
    hold(window, &desk, Vec::new(), RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    let (snapshot, _) = charted_funding("XRP");
    cx.update_window(window.into(), |_, window, cx| {
        funding.update(cx, |view, cx| view.set_chart(snapshot, cx));
        window.render_frame(cx);
        let drawn = funding.read(cx).drawn();
        assert_eq!(drawn.chart, "chart");
        assert_eq!(drawn.chart_note, "Last 90 days \u{b7} no position");
        // Four figures: no "would have" line.
        assert_eq!(drawn.figures.len(), 4);
        assert!(drawn.figures.iter().all(|f| !f.contains("would have")));
        assert!(
            drawn
                .legend
                .ends_with("above zero longs pay shorts, below zero shorts pay longs"),
            "{}",
            drawn.legend
        );
    })
    .unwrap();

    // A position is opened in it: the chart takes that side.
    hold(window, &desk, vec![short("XRP", "", 10_000)], RATES, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let drawn = funding.read(cx).drawn();
        assert_eq!(drawn.chart_note, "Last 90 days \u{b7} you are short");
        assert_eq!(drawn.figures.len(), 5);
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_chart_panel_loads_fails_and_shows_a_short_history_each_in_its_own_way(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(
        window,
        &desk,
        vec![position("SOL", "", 20_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    let now = now_ms();
    let show = |snapshot, window: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
        funding.update(cx, |view, cx| view.set_chart(snapshot, cx));
        window.render_frame(cx);
        funding.read(cx).drawn()
    };
    cx.update_window(window.into(), |_, window, cx| {
        // Nothing has arrived for the chosen market: loading, by name.
        assert_eq!(funding.read(cx).drawn().chart, "loading");
        assert!(window.try_find("funding-chart-loading").is_some());
        assert_eq!(
            funding.read(cx).drawn().chart_says,
            "Loading the last 90 days of funding for BTC"
        );

        // The newest 500 hours of the window: drawn, and said to be partial.
        let part = sample::snapshot(
            "BTC",
            Resolution::Day,
            sample::history(now, 500, false),
            true,
            false,
        );
        let drawn = show(part, window, cx);
        assert_eq!(drawn.chart, "part");
        assert_eq!(drawn.points, 20);
        assert!(
            drawn.chart_note.ends_with("earlier history is loading"),
            "{}",
            drawn.chart_note
        );
        assert_eq!(
            drawn.figures[2],
            format!("Positive: {} of 20 days", {
                let history = sample::history(now, 500, false);
                history.window(Resolution::Day).summary().unwrap().positive
            })
        );

        // It could not be loaded: said so, and the table is as it was.
        let rows_before = funding.read(cx).drawn().positions;
        let failed = sample::snapshot("BTC", Resolution::Day, History::default(), true, true);
        let drawn = show(failed, window, cx);
        assert_eq!(drawn.chart, "failed");
        assert!(window.try_find("funding-chart-failed").is_some());
        assert_eq!(drawn.positions, rows_before);
        assert!(drawn.chart_says.contains("could not be loaded"));
        assert!(
            drawn
                .figures_by_row
                .iter()
                .all(|row| row.contains("+0.0010%") && row.contains("-$")),
            "{:?}",
            drawn.figures_by_row
        );

        // A market listed 28 days ago: its days, and when they start.
        let young = sample::snapshot(
            "BTC",
            Resolution::Day,
            sample::history(now, 28 * 24, true),
            false,
            false,
        );
        let drawn = show(young, window, cx);
        assert_eq!(drawn.chart, "young");
        assert_eq!(drawn.points, 28);
        assert!(
            drawn.chart_note.contains("history starts "),
            "{}",
            drawn.chart_note
        );

        // The venue has no history for it at all.
        let mut none = History::default();
        none.merge_page(now, []);
        let drawn = show(
            sample::snapshot("BTC", Resolution::Day, none, false, false),
            window,
            cx,
        );
        assert_eq!(drawn.chart, "empty");
        assert!(window.try_find("funding-chart-empty").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_chart_is_shown_as_soon_as_the_history_held_is_the_chosen_markets(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    hold(
        window,
        &desk,
        vec![position("SOL", "", 20_000), position("BTC", "", 50_000)],
        RATES,
        cx,
    );
    let funding = go_to_funding(window, &desk, cx);
    let (btc, _) = charted_funding("BTC");
    let (sol, _) = charted_funding("SOL");
    cx.update_window(window.into(), |_, window, cx| {
        funding.update(cx, |view, cx| view.set_chart(btc.clone(), cx));
        window.render_frame(cx);
        assert_eq!(funding.read(cx).drawn().chart, "chart");

        // SOL is chosen while the history held is still BTC's: loading, and
        // never BTC's line under SOL's name.
        window.click(("funding-row", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(funding.read(cx).chosen_market().as_deref(), Some("SOL"));
        assert_eq!(funding.read(cx).drawn().chart, "loading");
        funding.update(cx, |view, cx| view.set_chart(sol, cx));
        window.render_frame(cx);
        assert_eq!(funding.read(cx).drawn().chart, "chart");

        // Back to BTC, whose history the feed still has: drawn at once.
        window.click(("funding-row", 0usize), cx);
        funding.update(cx, |view, cx| view.set_chart(btc, cx));
        window.render_frame(cx);
        assert_eq!(funding.read(cx).drawn().chart, "chart");
        assert!(window.try_find("funding-chart-loading").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn funding_with_watched_rows_and_a_chart_still_has_no_trading_control(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["DOGE", "XRP"]));
    hold(window, &desk, vec![short("BTC", "", 50_000)], RATES, cx);
    let funding = go_to_funding(window, &desk, cx);
    let (snapshot, _) = charted_funding("BTC");
    cx.update_window(window.into(), |_, window, cx| {
        funding.update(cx, |view, cx| view.set_chart(snapshot, cx));
        window.render_frame(cx);
        for forbidden in [
            "Trade", "Buy", "Sell", "Close", "Cancel", "Deposit", "Withdraw", "Transfer",
        ] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "a `{forbidden}` control is in a read-only section"
            );
        }
    })
    .unwrap();
}

// The Positions tab's actions: close at market or limit, Close all, and a
// position's take-profit and stop-loss.

use galata_desk_core::trade::sign::Tpsl;
use galata_desk_core::trade::{Holding, Level, Pricing, TpslNow, TpslRequest, TpslStep};

fn dec(text: &str) -> Decimal {
    text.parse().unwrap()
}

/// A position: signed size, marked at `mark`, entered at 152.
fn held(market: &str, dex: &str, size: &str, mark: &str, asset: Option<Asset>) -> PositionRow {
    PositionRow {
        asset,
        market: market.into(),
        dex: dex.into(),
        size: dec(size),
        value: dec(size).abs() * dec(mark),
        entry: dec("152"),
        mark: Some(dec(mark)),
        ..position(market, dex, 0)
    }
}

/// A long of 10 NVDA on `xyz`, marked at 154.20.
fn long_nvda() -> PositionRow {
    held("NVDA", "xyz", "10", "154.20", Some(NVDA))
}

/// A position-wide take-profit or stop-loss resting for NVDA on `xyz`.
fn level(oid: u64, kind: Tpsl, at: &str) -> OrderRow {
    OrderRow {
        dex: "xyz".into(),
        kind: match kind {
            Tpsl::Tp => "Take Profit Market".into(),
            Tpsl::Sl => "Stop Market".into(),
        },
        remaining: Decimal::ZERO,
        original: Decimal::ZERO,
        trigger_px: Some(dec(at)),
        tpsl: Some(kind),
        ..a_stop(oid)
    }
}

/// Open the Positions tab with these positions and orders and this
/// stand-in for the gate.
fn positions_tab(
    cx: &mut TestAppContext,
    staged: &Rc<Staged>,
    positions: Vec<PositionRow>,
    orders: Vec<OrderRow>,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<Portfolio>,
    tempfile::TempDir,
) {
    let (window, desk, dir) = open(cx, true);
    let view = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let view = desk.read(cx).portfolio_view().unwrap();
            view.update(cx, |view, cx| {
                view.use_trading(staged.clone());
                view.set_orders(orders, cx);
                view.set_positions(positions, cx);
            });
            window.within("holdings").click(1usize, cx);
            view
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    (window, desk, view, dir)
}

fn press_position(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    view: &Entity<Portfolio>,
    id: &str,
    key: u64,
) {
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| {
            view.press_position_button(id, key, window, cx)
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn on_view(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    view: &Entity<Portfolio>,
    f: impl FnOnce(&mut Portfolio, &mut gpui_kit::Window, &mut gpui_kit::Context<Portfolio>),
) {
    cx.update_window(window.into(), |_, window, cx| {
        view.update(cx, |view, cx| f(view, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

#[gpui_kit::test]
fn a_position_shows_its_tpsl_and_offers_limit_market_and_an_edit(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let positions = vec![long_nvda(), held("BTC", "", "-0.05", "91400", None)];
    let orders = vec![
        level(501, Tpsl::Tp, "160"),
        a_limit(1, "NVDA", Side::Buy, "12", "154.20"),
    ];
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, positions, orders);
    cx.update_window(window.into(), |_, window, cx| {
        let view = view.read(cx);
        // The take-profit is listed; no stop-loss is set. BTC has neither.
        assert_eq!(view.tpsl_cells(), ["160 / --", "-- / --"]);
        let controls = view.position_controls();
        let names: Vec<_> = controls.iter().take(3).map(|c| c.1.as_str()).collect();
        assert_eq!(
            names,
            [
                "Close at limit",
                "Close at market",
                "Edit take-profit and stop-loss"
            ]
        );
        assert!(controls.iter().take(3).all(|c| c.0 == 110_000 && !c.2));
        // A market not yet in the venue's list: shown, not usable.
        assert!(controls[3..].iter().all(|c| c.2));
        assert!(
            controls[3]
                .1
                .ends_with("this market's details are still loading")
        );
        // The buttons are in the window under their names.
        assert_eq!(window.find_all_by_label("Close at market").len(), 1);
        assert_eq!(
            window
                .find_all_by_label("Close all positions at market")
                .len(),
            1
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_positions_tab_says_why_it_cannot_act(cx: &mut TestAppContext) {
    let cases = [
        (
            Unavailable::NoKey(Network::Testnet),
            "An API key is needed to close positions or set take-profit and stop-loss",
        ),
        (
            Unavailable::KeyNotApproved,
            "The saved key cannot act for this account",
        ),
        (
            Unavailable::MainnetOff,
            "Trading on mainnet is switched off",
        ),
        (Unavailable::Overridden, "Position actions are off"),
    ];
    for (why, heading) in cases {
        let staged = Rc::new(Staged::new(Err(why)));
        let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
        cx.update_window(window.into(), |_, _, cx| {
            let view = view.read(cx);
            assert_eq!(view.positions_unavailable().as_deref(), Some(heading));
            assert!(view.position_controls().iter().all(|c| c.2));
            assert!(!view.can_close_all());
        })
        .unwrap();
        // A press arriving anyway opens nothing.
        press_position(cx, window, &view, "close-market", 110_000);
        cx.update_window(window.into(), |_, _, cx| {
            assert_eq!(view.read(cx).open_dialog(), None);
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn a_market_close_sends_the_size_and_worst_price_shown(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
    press_position(cx, window, &view, "close-market", 110_000);
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.open_dialog(), Some("close-market"));
        // The whole position, at no worse than 5% under the mid of 154.
        assert_eq!(
            view.close_state(cx),
            Some(("10 at 146.3".into(), "10".into(), true))
        );
    })
    .unwrap();

    // A quarter, rounded down to the size step.
    on_view(cx, window, &view, |v, w, cx| v.press_share(25, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).close_state(cx).unwrap().1, "2.5");
    })
    .unwrap();

    click(cx, window, "dialog-confirm");
    assert_eq!(
        *staged.closes.borrow(),
        vec![(
            Holding {
                asset: NVDA,
                size: dec("10"),
            },
            dec("2.5"),
            Pricing::Market {
                worst: dec("146.3")
            },
        )]
    );
}

#[gpui_kit::test]
fn the_close_form_names_what_to_fix(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    // A small long: 0.09 at 154.20 is $13.88.
    let small = held("NVDA", "xyz", "0.09", "154.20", Some(NVDA));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![small], vec![]);
    press_position(cx, window, &view, "close-limit", 110_000);
    let cases: &[(&str, &str, &str)] = &[
        ("0.1", "158", "Size is more than the position holds."),
        (
            "0.005",
            "158",
            "Size is too fine: at most 2 decimal places here.",
        ),
        ("0", "158", "Size must be a number above zero."),
        (
            "0.05",
            "158",
            "A partial close must be worth at least $10. Close all of it, or more of it.",
        ),
        (
            "0.09",
            "158.123",
            "Price is too precise: at most 5 significant figures and 4 decimal places here.",
        ),
    ];
    for (size, price, message) in cases {
        on_view(cx, window, &view, |v, w, cx| {
            v.type_close(size, price, w, cx)
        });
        cx.update_window(window.into(), |_, _, cx| {
            let (text, _, ok) = view.read(cx).close_state(cx).unwrap();
            assert_eq!(text, *message, "{size} at {price}");
            assert!(!ok);
        })
        .unwrap();
    }
    // The whole of it, worth under $10 at this price, may be closed.
    on_view(cx, window, &view, |v, w, cx| {
        v.type_close("0.09", "100", w, cx)
    });
    cx.update_window(window.into(), |_, _, cx| {
        assert!(view.read(cx).close_state(cx).unwrap().2);
    })
    .unwrap();

    // Backing out sends nothing.
    click(cx, window, "dialog-keep");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert!(staged.closes.borrow().is_empty());
}

#[gpui_kit::test]
fn a_limit_close_sends_the_price_typed(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged
        .answers
        .borrow_mut()
        .push_back(Ok(Answer::Each(vec![Outcome::Done {
            filled: None,
            resting: true,
        }])));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
    press_position(cx, window, &view, "close-limit", 110_000);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_close("4", "158.00", w, cx)
    });
    click(cx, window, "dialog-confirm");
    assert_eq!(
        staged.closes.borrow()[0].2,
        Pricing::Limit { price: dec("158") }
    );
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).orders_reported().unwrap().0,
            "Limit close placed: Sell 4 NVDA (xyz) at 158. It is listed in Open orders."
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_market_close_waits_for_the_price_and_can_ask_again(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.mid.borrow_mut() = Err("no connection".into());
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
    press_position(cx, window, &view, "close-market", 110_000);
    cx.update_window(window.into(), |_, _, cx| {
        let (text, _, ok) = view.read(cx).close_state(cx).unwrap();
        assert_eq!(text, "The market price could not be read: no connection.");
        assert!(!ok);
    })
    .unwrap();
    *staged.mid.borrow_mut() = Ok(Some(dec("150")));
    click(cx, window, "mid-retry");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).close_state(cx).unwrap().0,
            "10 at 142.5",
            "5% under the new mid"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn close_all_lists_every_position_and_sends_them_together(cx: &mut TestAppContext) {
    // Nothing to close: the control cannot be used.
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![], vec![]);
    cx.update_window(window.into(), |_, _, cx| {
        assert!(!view.read(cx).can_close_all());
    })
    .unwrap();

    let staged = Rc::new(Staged::new(Ok(())));
    let btc = Asset {
        number: 0,
        sz_decimals: 5,
        spot: false,
    };
    let short_btc = held("BTC", "", "-0.05", "154", Some(btc));
    let (window, _desk, view, _dir) =
        positions_tab(cx, &staged, vec![long_nvda(), short_btc], vec![]);
    on_view(cx, window, &view, |v, w, cx| v.press_close_all(w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.open_dialog(), Some("close-all"));
        assert_eq!(view.close_all_ready(), Some(true));
    })
    .unwrap();
    click(cx, window, "dialog-confirm");
    // The long sells no lower than 5% under the mid; the short buys no
    // higher than 5% over it.
    assert_eq!(
        *staged.close_alls.borrow(),
        vec![vec![
            (
                Holding {
                    asset: NVDA,
                    size: dec("10")
                },
                dec("146.3")
            ),
            (
                Holding {
                    asset: btc,
                    size: dec("-0.05")
                },
                dec("161.7")
            ),
        ]]
    );
}

#[gpui_kit::test]
fn the_tpsl_form_starts_from_the_listed_levels_and_checks_each(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let orders = vec![level(501, Tpsl::Tp, "160"), level(502, Tpsl::Sl, "148")];
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], orders);
    press_position(cx, window, &view, "edit-tpsl", 110_000);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("tpsl"));
        assert_eq!(
            view.read(cx).tpsl_state(cx),
            Some(("Change a level to send.".into(), false))
        );
    })
    .unwrap();
    let cases: &[(&str, &str, &str)] = &[
        (
            "150",
            "148",
            "A take-profit for a long must be above the mark.",
        ),
        (
            "160",
            "155",
            "A stop-loss for a long must be below the mark.",
        ),
        (
            "160.12345",
            "148",
            "Take-profit: Price is too precise: at most 5 significant figures and 4 decimal places here.",
        ),
        ("160", "x", "Stop-loss: Price must be a number above zero."),
    ];
    for (tp, sl, message) in cases {
        on_view(cx, window, &view, |v, w, cx| v.type_tpsl(tp, sl, w, cx));
        cx.update_window(window.into(), |_, _, cx| {
            assert_eq!(
                view.read(cx).tpsl_state(cx),
                Some((message.to_string(), false)),
                "{tp} / {sl}"
            );
        })
        .unwrap();
    }
    // Move the stop and clear the take-profit.
    on_view(cx, window, &view, |v, w, cx| v.type_tpsl("", "150", w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).tpsl_state(cx),
            Some(("take-profit removed, stop-loss at 150".into(), true))
        );
    })
    .unwrap();
    click(cx, window, "dialog-confirm");
    assert_eq!(
        *staged.tpsl_changes.borrow(),
        vec![(
            Holding {
                asset: NVDA,
                size: dec("10")
            },
            TpslNow {
                tp: Some(501),
                sl: Some(502)
            },
            Level::Remove,
            Level::At(dec("150")),
        )]
    );
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).orders_reported().unwrap().0,
            "Stop-loss moved to 150, take-profit removed on NVDA (xyz)."
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_new_level_on_a_short_is_placed(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let short = held("NVDA", "xyz", "-10", "154.20", Some(NVDA));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![short], vec![]);
    press_position(cx, window, &view, "edit-tpsl", 110_000);
    // For a short the take-profit is below the mark and the stop above it.
    on_view(cx, window, &view, |v, w, cx| {
        v.type_tpsl("148", "160", w, cx)
    });
    click(cx, window, "dialog-confirm");
    let changes = staged.tpsl_changes.borrow();
    assert_eq!(changes[0].1, TpslNow::default());
    assert_eq!(
        (changes[0].2, changes[0].3),
        (Level::At(dec("148")), Level::At(dec("160")))
    );
}

#[gpui_kit::test]
fn one_action_at_a_time_across_both_tabs(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged.hold.set(true);
    let orders = vec![a_limit(1, "NVDA", Side::Buy, "12", "154.20")];
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], orders);
    staged.hold.set(false);
    press_position(cx, window, &view, "close-limit", 110_000);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_close("4", "158", w, cx)
    });
    staged.hold.set(true);
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.positions_pending(), [110_000]);
        assert!(view.position_controls().iter().all(|c| c.2));
        // The Open orders tab waits too.
        assert!(view.order_controls().iter().all(|c| c.2));
        assert!(!view.can_cancel_all());
    })
    .unwrap();
}

#[gpui_kit::test]
fn each_close_outcome_is_said_and_both_lists_are_reloaded(cx: &mut TestAppContext) {
    let filled = |size: &str, avg: &str| Outcome::Done {
        filled: Some(Traded {
            size: Some(dec(size)),
            avg_price: Some(dec(avg)),
        }),
        resting: false,
    };
    let cases: Vec<(Result<Answer, Unavailable>, &str, Option<&str>)> = vec![
        (
            Ok(Answer::Each(vec![filled("10", "153.91")])),
            "Closed 10 NVDA (xyz) at an average of 153.91.",
            None,
        ),
        (
            Ok(Answer::Each(vec![filled("6", "153.91")])),
            "Closed 6 of 10 NVDA (xyz) at an average of 153.91.",
            Some("The rest did not fill within the price limit of 146.3; nothing is left resting."),
        ),
        (
            Ok(Answer::Each(vec![Outcome::Refused(
                "Order could not immediately match against any resting orders. asset=110000".into(),
            )])),
            "Not closed: nothing on the book could fill within the price limit of 146.3.",
            Some(
                "Hyperliquid said: Order could not immediately match against any resting orders. asset=110000",
            ),
        ),
        (
            Ok(Answer::Unknown("the request timed out".into())),
            "No answer from Hyperliquid (the request timed out). The close may or may not have gone through; positions and orders are being reloaded.",
            None,
        ),
    ];
    for (answer, text, more) in cases {
        let staged = Rc::new(Staged::new(Ok(())));
        staged.answers.borrow_mut().push_back(answer);
        let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
        press_position(cx, window, &view, "close-market", 110_000);
        click(cx, window, "dialog-confirm");
        cx.update_window(window.into(), |_, _, cx| {
            let view = view.read(cx);
            let (said, extra, _) = view.orders_reported().unwrap();
            assert_eq!(said, text);
            assert_eq!(extra.as_deref(), more);
            assert_eq!(view.orders_refreshes(), 1);
            assert!(view.positions_pending().is_empty());
        })
        .unwrap();
    }
}

#[gpui_kit::test]
fn close_all_and_tpsl_say_what_was_not_done(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged.answers.borrow_mut().push_back(Ok(Answer::Each(vec![
        Outcome::Done {
            filled: Some(Traded {
                size: Some(dec("10")),
                avg_price: Some(dec("153.9")),
            }),
            resting: false,
        },
        Outcome::Refused(
            "Order could not immediately match against any resting orders. asset=0".into(),
        ),
    ])));
    let btc = Asset {
        number: 0,
        sz_decimals: 5,
        spot: false,
    };
    let (window, _desk, view, _dir) = positions_tab(
        cx,
        &staged,
        vec![long_nvda(), held("BTC", "", "0.05", "154", Some(btc))],
        vec![level(502, Tpsl::Sl, "148")],
    );
    on_view(cx, window, &view, |v, w, cx| v.press_close_all(w, cx));
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        let (text, more, problem) = view.read(cx).orders_reported().unwrap();
        assert_eq!(text, "Closed 1 of 2 positions.");
        assert_eq!(
            more.as_deref(),
            Some("BTC not closed (nothing filled within its price limit)")
        );
        assert!(problem);
    })
    .unwrap();

    // A TP/SL change whose move was refused while its new level was set.
    staged.tpsl_answers.borrow_mut().push_back(vec![
        TpslStep {
            kind: TpslRequest::Move,
            levels: vec![Tpsl::Sl],
            answer: Ok(Answer::Refused(
                "Cannot modify canceled or filled order".into(),
            )),
        },
        TpslStep {
            kind: TpslRequest::Place,
            levels: vec![Tpsl::Tp],
            answer: Ok(Answer::Each(vec![Outcome::DONE])),
        },
    ]);
    press_position(cx, window, &view, "edit-tpsl", 110_000);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_tpsl("160", "150", w, cx)
    });
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        let (text, more, problem) = view.read(cx).orders_reported().unwrap();
        assert_eq!(text, "Take-profit set at 160 on NVDA (xyz).");
        assert_eq!(
            more.as_deref(),
            Some("Not done: stop-loss moved to 150 (Cannot modify canceled or filled order).")
        );
        assert!(problem);
    })
    .unwrap();
}

// The order ticket, opened from the Markets panel.

use galata_desk_core::trade::NewOrder;
use galata_desk_core::trade::rules::SizeUnit;

const HYPE_ASSET: Asset = Asset {
    number: 4,
    sz_decimals: 2,
    spot: false,
};

/// Open the ticket for HYPE with these positions and this stand-in.
fn ticket(
    cx: &mut TestAppContext,
    staged: &Rc<Staged>,
    positions: Vec<PositionRow>,
    buy: bool,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<Portfolio>,
    tempfile::TempDir,
) {
    let (window, desk, view, dir) = positions_tab(cx, staged, positions, vec![]);
    on_view(cx, window, &view, |v, w, cx| {
        v.open_ticket(MarketId::new("HYPE"), buy, w, cx)
    });
    (window, desk, view, dir)
}

fn ticket_says(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    view: &Entity<Portfolio>,
) -> (String, bool) {
    cx.update_window(window.into(), |_, _, cx| {
        view.read(cx).ticket_state(cx).unwrap()
    })
    .unwrap()
}

#[gpui_kit::test]
fn the_ticket_opens_in_usd_and_takes_the_coin_too(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.ticket_open(), Some(("HYPE".into(), true, true)));
        assert_eq!(view.ticket_unit(), Some(SizeUnit::Usd));
    })
    .unwrap();
    // $100 at the worst price, 5% over the mid of 154: 0.61 HYPE.
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("100", "", w, cx)
    });
    assert_eq!(
        ticket_says(cx, window, &view),
        ("buy 0.61 at 161.7".into(), true)
    );
    // In the coin.
    click(cx, window, "ticket-unit-coin");
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("1.5", "", w, cx)
    });
    assert_eq!(
        ticket_says(cx, window, &view),
        ("buy 1.5 at 161.7".into(), true)
    );
}

#[gpui_kit::test]
fn the_ticket_names_what_to_fix(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    let cases: &[(Option<SizeUnit>, bool, &str, &str, &str)] = &[
        (
            None,
            true,
            "5",
            "",
            "The order would be worth less than the $10 minimum.",
        ),
        (
            Some(SizeUnit::Coin),
            true,
            "1.505",
            "",
            "Size is too fine: at most 2 decimal places here.",
        ),
        (
            Some(SizeUnit::Coin),
            true,
            "31",
            "",
            "Not enough to trade this size: at most 30 here.",
        ),
        (
            Some(SizeUnit::Coin),
            false,
            "2",
            "x",
            "Price must be a number above zero.",
        ),
    ];
    for (unit, at_market, size, price, message) in cases {
        on_view(cx, window, &view, |v, _, cx| {
            v.set_ticket_for_test(None, Some(*at_market), *unit, None, None, cx)
        });
        on_view(cx, window, &view, |v, w, cx| {
            v.type_ticket(size, price, w, cx)
        });
        assert_eq!(
            ticket_says(cx, window, &view),
            (message.to_string(), false),
            "{size} {price}"
        );
    }
    // Reduce only with no position, then a take-profit on the wrong side.
    on_view(cx, window, &view, |v, _, cx| {
        v.set_ticket_for_test(None, Some(true), Some(SizeUnit::Coin), Some(true), None, cx)
    });
    on_view(cx, window, &view, |v, w, cx| v.type_ticket("1", "", w, cx));
    assert_eq!(
        ticket_says(cx, window, &view),
        ("There is no position to reduce.".into(), false)
    );
    on_view(cx, window, &view, |v, _, cx| {
        v.set_ticket_for_test(None, None, None, Some(false), Some(true), cx)
    });
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket_tpsl("150", "140", w, cx)
    });
    assert_eq!(
        ticket_says(cx, window, &view),
        (
            "A take-profit for a buy must be above its price.".into(),
            false
        )
    );
    // Backing out sends nothing.
    click(cx, window, "dialog-keep");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).ticket_open(), None)
    })
    .unwrap();
    assert!(staged.placed.borrow().is_empty());
}

#[gpui_kit::test]
fn a_share_fills_in_part_of_what_can_be_traded(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    // Half of the 30 HYPE the venue allows, in USD at the mid of 154.
    on_view(cx, window, &view, |v, w, cx| v.ticket_share(50, w, cx));
    let text = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, _, cx| {
            view.read(cx).ticket_size_text(cx).unwrap()
        })
        .unwrap()
    };
    assert_eq!(text(cx), "2310");
    click(cx, window, "ticket-unit-coin");
    on_view(cx, window, &view, |v, w, cx| v.ticket_share(50, w, cx));
    assert_eq!(text(cx), "15");
}

#[gpui_kit::test]
fn the_order_sent_is_the_one_shown(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("100", "", w, cx)
    });
    click(cx, window, "dialog-confirm");
    // A limit sell with a take-profit and stop-loss, in the coin.
    on_view(cx, window, &view, |v, w, cx| {
        v.dismiss_dialog(cx);
        v.open_ticket(MarketId::new("HYPE"), false, w, cx)
    });
    on_view(cx, window, &view, |v, _, cx| {
        v.set_ticket_for_test(
            None,
            Some(false),
            Some(SizeUnit::Coin),
            None,
            Some(true),
            cx,
        )
    });
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("2", "160", w, cx);
        v.type_ticket_tpsl("150", "170", w, cx);
    });
    click(cx, window, "dialog-confirm");
    assert_eq!(
        *staged.placed.borrow(),
        vec![
            NewOrder {
                asset: HYPE_ASSET,
                buy: true,
                size: dec("0.61"),
                pricing: Pricing::Market {
                    worst: dec("161.7")
                },
                reduce_only: false,
                take_profit: None,
                stop_loss: None,
            },
            NewOrder {
                asset: HYPE_ASSET,
                buy: false,
                size: dec("2"),
                pricing: Pricing::Limit { price: dec("160") },
                reduce_only: false,
                take_profit: Some(dec("150")),
                stop_loss: Some(dec("170")),
            },
        ]
    );
}

#[gpui_kit::test]
fn the_ticket_says_why_it_cannot_send(cx: &mut TestAppContext) {
    let cases = [
        (
            Unavailable::NoKey(Network::Testnet),
            "An API key is needed to place orders",
        ),
        (
            Unavailable::KeyNotApproved,
            "The saved key cannot act for this account",
        ),
        (
            Unavailable::MainnetOff,
            "Trading on mainnet is switched off",
        ),
        (Unavailable::Overridden, "Placing orders is off"),
    ];
    for (why, heading) in cases {
        let staged = Rc::new(Staged::new(Err(why)));
        let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
        on_view(cx, window, &view, |v, w, cx| {
            v.type_ticket("100", "", w, cx)
        });
        assert_eq!(ticket_says(cx, window, &view), (heading.to_string(), false));
        click(cx, window, "dialog-confirm");
        assert!(staged.placed.borrow().is_empty());
    }
    // What the venue could not tell: the account, then the price.
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.active.borrow_mut() = Err("no connection".into());
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("100", "", w, cx)
    });
    assert_eq!(
        ticket_says(cx, window, &view),
        (
            "What this account can trade could not be read: no connection.".into(),
            false
        )
    );
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.mid.borrow_mut() = Err("no connection".into());
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("100", "", w, cx)
    });
    assert_eq!(
        ticket_says(cx, window, &view),
        (
            "The market price could not be read: no connection.".into(),
            false
        )
    );
    // Asking again, once the venue answers.
    *staged.mid.borrow_mut() = Ok(Some(dec("154")));
    click(cx, window, "ticket-retry");
    assert!(ticket_says(cx, window, &view).1);
}

#[gpui_kit::test]
fn each_answer_is_said_in_the_ticket_and_both_lists_are_reloaded(cx: &mut TestAppContext) {
    let filled = |size: &str| Outcome::Done {
        filled: Some(Traded {
            size: Some(dec(size)),
            avg_price: Some(dec("154.2")),
        }),
        resting: false,
    };
    let cases: Vec<(Result<Answer, Unavailable>, &str, Option<&str>)> = vec![
        (
            Ok(Answer::Each(vec![filled("0.61")])),
            "Bought 0.61 HYPE at an average of 154.2.",
            None,
        ),
        (
            Ok(Answer::Each(vec![filled("0.3")])),
            "Bought 0.3 of 0.61 HYPE at an average of 154.2.",
            Some("The rest did not fill within the price limit of 161.7; nothing is left resting."),
        ),
        (
            Ok(Answer::Each(vec![Outcome::Refused(
                "Order could not immediately match against any resting orders. asset=4".into(),
            )])),
            "Not filled: nothing on the book within the price limit of 161.7.",
            Some(
                "Hyperliquid said: Order could not immediately match against any resting orders. asset=4",
            ),
        ),
        (
            Ok(Answer::Unknown("the request timed out".into())),
            "No answer from Hyperliquid (the request timed out). The order may or may not have gone through; positions and orders are being reloaded.",
            None,
        ),
    ];
    for (answer, text, more) in cases {
        let staged = Rc::new(Staged::new(Ok(())));
        staged.answers.borrow_mut().push_back(answer);
        let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
        on_view(cx, window, &view, |v, w, cx| {
            v.type_ticket("100", "", w, cx)
        });
        click(cx, window, "dialog-confirm");
        cx.update_window(window.into(), |_, _, cx| {
            let view = view.read(cx);
            // The ticket stays open with the answer.
            assert!(view.ticket_open().is_some());
            let (said, extra, _) = view.ticket_outcome().unwrap();
            assert_eq!(said, text);
            assert_eq!(extra.as_deref(), more);
            assert_eq!(view.orders_refreshes(), 1);
        })
        .unwrap();
        click(cx, window, "ticket-done");
        cx.update_window(window.into(), |_, _, cx| {
            assert_eq!(view.read(cx).ticket_open(), None)
        })
        .unwrap();
    }
    // A resting limit order.
    let staged = Rc::new(Staged::new(Ok(())));
    staged
        .answers
        .borrow_mut()
        .push_back(Ok(Answer::Each(vec![Outcome::Done {
            filled: None,
            resting: true,
        }])));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], false);
    on_view(cx, window, &view, |v, _, cx| {
        v.set_ticket_for_test(None, Some(false), Some(SizeUnit::Coin), None, None, cx)
    });
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("2", "160", w, cx)
    });
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            view.read(cx).ticket_outcome().unwrap().0,
            "Order placed: Sell 2 HYPE at 160. It is resting and listed in Open orders."
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_order_with_the_venue_holds_every_other_action(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![long_nvda()], true);
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("100", "", w, cx)
    });
    staged.hold.set(true);
    click(cx, window, "dialog-confirm");
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.positions_pending(), [4]);
        assert!(view.position_controls().iter().all(|c| c.2));
        assert!(!view.can_close_all());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_panel_opens_the_ticket_on_the_side_pressed(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["HYPE", "BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |p, _| p.use_trading(Rc::new(Staged::new(Ok(())))));
        markets.update(cx, |view, cx| {
            view.set_snapshot(priced_snapshot(&["HYPE", "BTC"]), cx)
        });
        window.render_frame(cx);
        // Named with the market for assistive technology.
        assert_eq!(window.find_all_by_label("Buy HYPE").len(), 1);
        assert_eq!(window.find_all_by_label("Sell HYPE").len(), 1);
    })
    .unwrap();
    click(cx, window, "trade-sell");
    cx.update_window(window.into(), |_, window, cx| {
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        assert_eq!(
            portfolio.read(cx).ticket_open(),
            Some(("HYPE".into(), false, true))
        );
        // Drawn over the Markets section.
        assert!(window.try_find("ticket-dialog").is_some());
    })
    .unwrap();

    // A market the venue no longer lists offers neither button.
    cx.update_window(window.into(), |_, window, cx| {
        let portfolio = desk.read(cx).portfolio_view().unwrap();
        portfolio.update(cx, |p, cx| p.dismiss_dialog(cx));
        let mut snapshot = priced_snapshot(&["HYPE"]);
        snapshot.tiles[0].unavailable = true;
        markets.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        window.render_frame(cx);
        assert!(window.try_find("trade-buy").is_none());
    })
    .unwrap();
}

// ---- Quick order sizes in Settings ----

use galata_desk_core::store::QuickSizes;

/// A window tall enough to show all of Settings without scrolling.
fn open_tall(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    open_sized(cx, false, size(px(1320.), px(1400.)))
}

/// Open Settings and hand back its view.
fn open_settings(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
) -> Entity<crate::settings::Settings> {
    click(cx, window, "settings");
    cx.update_window(window.into(), |_, _, cx| {
        desk.read(cx).settings_view().unwrap()
    })
    .unwrap()
}

fn type_quick(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    settings: &Entity<crate::settings::Settings>,
    ix: usize,
    text: &str,
) {
    cx.update_window(window.into(), |_, window, cx| {
        settings.update(cx, |view, cx| view.type_quick(ix, text, window, cx));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        settings.update(cx, |_, cx| cx.notify());
        window.render_frame(cx)
    })
    .unwrap();
}

fn remove_quick(cx: &mut TestAppContext, window: WindowHandle<Root>, ix: usize) {
    cx.update_window(window.into(), |_, window, cx| {
        window.click(("quick-size-remove", ix), cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn quick_in_use(cx: &mut TestAppContext) -> Vec<u64> {
    cx.update(|cx| Core::get(cx).quick_sizes.amounts().to_vec())
}

#[gpui_kit::test]
fn quick_sizes_start_at_the_default_and_a_changed_list_is_used_and_saved(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_tall(cx);
    assert_eq!(quick_in_use(cx), [5_000, 10_000, 25_000]);
    let settings = open_settings(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            settings.read(cx).quick_field_texts(cx),
            ["5000", "10000", "25000"]
        );
        assert_eq!(window.find_all_by_label("Quick size 1").len(), 1);
        assert_eq!(window.find_all_by_label("Remove Quick size 3").len(), 1);
    })
    .unwrap();

    type_quick(cx, window, &settings, 0, "2000");
    type_quick(cx, window, &settings, 1, "20000");
    remove_quick(cx, window, 2);
    assert_eq!(quick_in_use(cx), [2_000, 20_000]);
    let stored = Store::at(dir.path()).settings();
    assert_eq!(stored.quick_sizes.amounts(), [2_000, 20_000]);
    // Nothing else changed.
    assert_eq!(stored.network, Network::Mainnet);
    assert!(!stored.allow_mainnet_trading);
}

#[gpui_kit::test]
fn quick_sizes_are_remembered_across_launches(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    Store::at(dir.path())
        .save_settings(&galata_desk_core::store::Settings {
            quick_sizes: QuickSizes::check(&["2000", "20000"]).unwrap(),
            watcher_url: None,
            ..Default::default()
        })
        .unwrap();
    let (_window, _desk) = open_at(
        cx,
        dir.path(),
        false,
        size(
            px(crate::DEFAULT_WINDOW_WIDTH),
            px(crate::DEFAULT_WINDOW_HEIGHT),
        ),
    );
    assert_eq!(quick_in_use(cx), [2_000, 20_000]);
}

#[gpui_kit::test]
fn a_quick_size_that_breaks_a_rule_says_why_and_changes_nothing(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_tall(cx);
    let settings = open_settings(cx, window, &desk);
    let before = std::fs::read_to_string(dir.path().join("settings.json")).ok();
    for (text, reasons) in [
        ("2500.5", [Some("Whole dollars only."), None, None]),
        ("5", [Some("At least $10."), None, None]),
        // Typed while $10k is listed: both fields say so.
        (
            "10000",
            [Some("Already listed."), Some("Already listed."), None],
        ),
    ] {
        type_quick(cx, window, &settings, 0, text);
        cx.update_window(window.into(), |_, _, cx| {
            assert_eq!(settings.read(cx).quick_problems(cx), reasons, "{text}");
        })
        .unwrap();
        // The last valid list stays in use, and the file is untouched.
        assert_eq!(quick_in_use(cx), [5_000, 10_000, 25_000], "{text}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("settings.json")).ok(),
            before,
            "{text}"
        );
    }
    // Fixed: used at once.
    type_quick(cx, window, &settings, 0, "1000");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(settings.read(cx).quick_problems(cx), [None, None, None]);
    })
    .unwrap();
    assert_eq!(quick_in_use(cx), [1_000, 10_000, 25_000]);
}

#[gpui_kit::test]
fn add_stops_at_four_remove_at_one_and_reset_restores_the_default(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_tall(cx);
    let settings = open_settings(cx, window, &desk);
    click(cx, window, "quick-size-add");
    // A new field is empty: not valid yet, says nothing, and the list in
    // use is unchanged.
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(settings.read(cx).quick_field_texts(cx).len(), 4);
        assert_eq!(settings.read(cx).quick_problems(cx), [None; 4]);
        // Four fields: no more can be added.
        assert!(window.try_find("quick-size-add").is_none());
    })
    .unwrap();
    assert_eq!(quick_in_use(cx), [5_000, 10_000, 25_000]);
    type_quick(cx, window, &settings, 3, "1000");
    assert_eq!(quick_in_use(cx), [1_000, 5_000, 10_000, 25_000]);

    for _ in 0..3 {
        remove_quick(cx, window, 0);
    }
    // The field typed last is the one left.
    assert_eq!(quick_in_use(cx), [1_000]);
    // One left: it cannot be removed.
    remove_quick(cx, window, 0);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(settings.read(cx).quick_field_texts(cx), ["1000"]);
    })
    .unwrap();
    assert_eq!(quick_in_use(cx), [1_000]);

    click(cx, window, "quick-size-reset");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(
            settings.read(cx).quick_field_texts(cx),
            ["5000", "10000", "25000"]
        );
    })
    .unwrap();
    assert_eq!(quick_in_use(cx), [5_000, 10_000, 25_000]);
    assert_eq!(
        Store::at(dir.path()).settings().quick_sizes,
        QuickSizes::default()
    );
}

// ---- Quick sizes in the order ticket ----

use crate::portfolio::QuickButton;

/// A stand-in account that can trade up to `buy` and `sell` of the coin.
fn account_trading(buy: &str, sell: &str) -> galata_desk_core::venue::ActiveAsset {
    serde_json::from_value(serde_json::json!({
        "leverage": { "type": "cross", "value": 5 },
        "maxTradeSzs": [buy, sell],
        "availableToTrade": ["3737.4", "3737.4"],
        "markPx": "154"
    }))
    .unwrap()
}

fn press_quick(cx: &mut TestAppContext, window: WindowHandle<Root>, amount: u64) {
    cx.update_window(window.into(), |_, window, cx| {
        window.click(
            gpui_kit::ElementId::NamedInteger("ticket-quick".into(), amount),
            cx,
        )
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

fn quick_buttons(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    view: &Entity<Portfolio>,
) -> (Vec<QuickButton>, Option<u64>) {
    cx.update_window(window.into(), |_, _, cx| {
        view.read(cx).ticket_quick_buttons(cx).unwrap()
    })
    .unwrap()
}

/// Which quick sizes are marked, and which can be pressed.
fn marked_and_usable(buttons: &[QuickButton]) -> (Vec<u64>, Vec<u64>) {
    (
        buttons
            .iter()
            .filter(|b| b.marked)
            .map(|b| b.amount)
            .collect(),
        buttons
            .iter()
            .filter(|b| b.usable)
            .map(|b| b.amount)
            .collect(),
    )
}

#[gpui_kit::test]
fn a_quick_size_fills_in_dollars_from_the_coin_and_stays_marked_while_it_matches(
    cx: &mut TestAppContext,
) {
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.active.borrow_mut() = Ok(account_trading("1000", "1000"));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    click(cx, window, "ticket-unit-coin");
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("1.5", "", w, cx)
    });
    // In the coin, 1.5 is not a dollar amount: nothing is marked.
    let (buttons, most) = quick_buttons(cx, window, &view);
    assert_eq!(
        marked_and_usable(&buttons),
        (vec![], vec![5_000, 10_000, 25_000])
    );
    assert_eq!(most, None);

    press_quick(cx, window, 5_000);
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.ticket_unit(), Some(SizeUnit::Usd));
        assert_eq!(view.ticket_size_text(cx).as_deref(), Some("5000"));
    })
    .unwrap();
    // $5,000 at the worst price of 161.7: 30.92 HYPE.
    assert_eq!(
        ticket_says(cx, window, &view),
        ("buy 30.92 at 161.7".into(), true)
    );
    assert_eq!(
        marked_and_usable(&quick_buttons(cx, window, &view).0).0,
        [5_000]
    );

    press_quick(cx, window, 10_000);
    assert_eq!(
        marked_and_usable(&quick_buttons(cx, window, &view).0).0,
        [10_000]
    );
    // Typing another size: nothing marked. Typing the amount: marked again.
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("7500", "", w, cx)
    });
    assert!(
        marked_and_usable(&quick_buttons(cx, window, &view).0)
            .0
            .is_empty()
    );
    on_view(cx, window, &view, |v, w, cx| {
        v.type_ticket("25000", "", w, cx)
    });
    assert_eq!(
        marked_and_usable(&quick_buttons(cx, window, &view).0).0,
        [25_000]
    );
    // The same digits in the coin are not dollars.
    click(cx, window, "ticket-unit-coin");
    assert!(
        marked_and_usable(&quick_buttons(cx, window, &view).0)
            .0
            .is_empty()
    );
}

#[gpui_kit::test]
fn quick_sizes_above_what_the_account_can_trade_cannot_be_pressed(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    // Buying at the worst price of 161.7, 49.48 HYPE is about $8,000; selling
    // at 146.3, 200 HYPE is $29,260.
    *staged.active.borrow_mut() = Ok(account_trading("49.48", "200"));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    let (buttons, most) = quick_buttons(cx, window, &view);
    assert_eq!(marked_and_usable(&buttons).1, [5_000]);
    assert_eq!(most, Some(8_000));
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find_all_by_label("Size $5k").len(), 1);
        assert_eq!(
            window
                .find_all_by_label("Size $10k, more than this account can trade")
                .len(),
            1
        );
        assert_eq!(
            window
                .find_all_by_label("Size $25k, more than this account can trade")
                .len(),
            1
        );
    })
    .unwrap();
    // Pressing one that is out of reach does nothing.
    press_quick(cx, window, 10_000);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).ticket_size_text(cx).as_deref(), Some(""));
    })
    .unwrap();
    press_quick(cx, window, 5_000);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).ticket_size_text(cx).as_deref(), Some("5000"));
    })
    .unwrap();

    // The sell side allows more: every button can be pressed, and the line
    // goes.
    click(cx, window, "ticket-sell");
    let (buttons, most) = quick_buttons(cx, window, &view);
    assert_eq!(marked_and_usable(&buttons).1, [5_000, 10_000, 25_000]);
    assert_eq!(most, None);
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find_all_by_label("Size $25k").len(), 1);
    })
    .unwrap();
}

#[gpui_kit::test]
fn every_quick_size_can_be_pressed_before_the_venue_answers(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.active.borrow_mut() = Ok(account_trading("1", "1"));
    staged.hold.set(true);
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    let (buttons, most) = quick_buttons(cx, window, &view);
    assert_eq!(marked_and_usable(&buttons).1, [5_000, 10_000, 25_000]);
    assert_eq!(most, None);
    press_quick(cx, window, 25_000);
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).ticket_size_text(cx).as_deref(), Some("25000"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_ticket_offers_the_quick_sizes_chosen_in_settings(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    *staged.active.borrow_mut() = Ok(account_trading("1000", "1000"));
    let (window, _desk, view, _dir) = ticket(cx, &staged, vec![], true);
    cx.update(|cx| {
        cx.global_mut::<Core>().quick_sizes = QuickSizes::check(&["20000", "2000"]).unwrap();
    });
    on_view(cx, window, &view, |_, _, cx| cx.notify());
    let (buttons, _) = quick_buttons(cx, window, &view);
    assert_eq!(
        buttons.iter().map(|b| b.amount).collect::<Vec<_>>(),
        [2_000, 20_000]
    );
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find_all_by_label("Size $2k").len(), 1);
        assert_eq!(window.find_all_by_label("Size $20k").len(), 1);
        assert!(window.find_all_by_label("Size $5k").is_empty());
    })
    .unwrap();
}

// ---- The History section -------------------------------------------------

mod watcher;

mod history_section {
    use super::*;
    use crate::history::History;
    use galata_desk_core::history::Period;
    use galata_desk_core::history::feed::{HistorySnapshot, OrdersHeld, Tab};
    use galata_desk_core::history::loader::Paged;
    use galata_desk_core::history::orders::Status;
    use galata_desk_core::venue::{
        FundingDelta, HistoricalOrder, LedgerDelta, LedgerRow, OpenOrder, UserFundingRow,
        VenueError,
    };

    const MINUTE_MS: u64 = 60_000;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    /// Click History in the section switch and return the view.
    fn go_to_history(
        window: WindowHandle<Root>,
        desk: &Entity<Desk>,
        cx: &mut TestAppContext,
    ) -> Entity<History> {
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("sections").click(3usize, cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            desk.read(cx)
                .history_view()
                .expect("an account is connected")
        })
        .unwrap()
    }

    #[allow(clippy::too_many_arguments)]
    fn fill(
        id: u64,
        oid: u64,
        coin: &str,
        dir: &str,
        price: &str,
        size: &str,
        fee: &str,
        taker: bool,
        pnl: &str,
        minutes_ago: u64,
    ) -> Fill {
        Fill {
            coin: coin.into(),
            side: if matches!(dir, "Open Long" | "Close Short") {
                FillSide::Bought
            } else {
                FillSide::Sold
            },
            price: d(price),
            size: d(size),
            time_ms: now_ms() - minutes_ago * MINUTE_MS,
            realised: d(pnl),
            fee: d(fee),
            id,
            dir: dir.into(),
            taker,
            oid,
            hash: format!("0x{id:064x}"),
        }
    }

    /// The spec's fills: ANT opened and part closed, and one NVDA order in
    /// five fills.
    fn the_fills() -> Vec<Fill> {
        let mut fills = vec![
            fill(
                1,
                61,
                "xyz:ANT",
                "Open Short",
                "105.00",
                "28.57",
                "0.09",
                false,
                "0",
                10,
            ),
            fill(
                2,
                63,
                "xyz:ANT",
                "Close Short",
                "101.00",
                "14.85",
                "0.04",
                false,
                "21.24",
                20,
            ),
            fill(
                3,
                64,
                "xyz:ANT",
                "Open Short",
                "105.00",
                "23.80",
                "0.07",
                true,
                "0",
                30,
            ),
        ];
        for (i, size) in ["0.367", "5.970", "6.650", "0.347", "6.650"]
            .iter()
            .enumerate()
        {
            let fee = if size.starts_with("0.") { "0" } else { "0.03" };
            fills.push(fill(
                10 + i as u64,
                68,
                "xyz:NVDA",
                "Open Long",
                "150.12",
                size,
                fee,
                true,
                "0",
                3 * 24 * 60 + i as u64,
            ));
        }
        fills
    }

    #[allow(clippy::too_many_arguments)]
    fn order(
        oid: u64,
        status: &str,
        minutes_ago: u64,
        sz: &str,
        orig: &str,
        kind: &str,
        reduce: bool,
        side: Side,
    ) -> HistoricalOrder {
        let at = now_ms() - minutes_ago * MINUTE_MS;
        HistoricalOrder {
            order: OpenOrder {
                coin: "xyz:ANT".into(),
                side,
                limit_px: d("105"),
                sz: d(sz),
                orig_sz: d(orig),
                oid,
                timestamp: at - 60 * MINUTE_MS,
                order_type: kind.into(),
                reduce_only: reduce,
                is_trigger: kind != "Limit",
                trigger_condition: if kind == "Limit" {
                    "N/A".into()
                } else {
                    "Price below 85".into()
                },
                trigger_px: if kind == "Limit" {
                    Decimal::ZERO
                } else {
                    d("85")
                },
                is_position_tpsl: orig == "0",
                tif: None,
            },
            status: status.into(),
            status_timestamp: at,
        }
    }

    fn the_orders() -> HistorySnapshot {
        let mut snapshot = HistorySnapshot::default();
        snapshot.orders.data = Some(OrdersHeld {
            rows: vec![
                order(1, "filled", 5, "0", "28.57", "Limit", false, Side::Sell),
                order(1, "open", 30, "28.57", "28.57", "Limit", false, Side::Sell),
                order(
                    2,
                    "reduceOnlyCanceled",
                    40,
                    "0",
                    "0",
                    "Take Profit Market",
                    true,
                    Side::Buy,
                ),
                order(3, "open", 50, "15", "15", "Limit", false, Side::Buy),
            ],
            shown_from: None,
        });
        snapshot
    }

    fn payment(
        minutes_ago: u64,
        coin: &str,
        szi: &str,
        usdc: &str,
        rate: &str,
        n: u32,
    ) -> UserFundingRow {
        UserFundingRow {
            time: now_ms() - minutes_ago * MINUTE_MS,
            delta: FundingDelta {
                coin: coin.into(),
                usdc: d(usdc),
                szi: d(szi),
                funding_rate: d(rate),
                n_samples: n,
            },
        }
    }

    fn the_payments() -> HistorySnapshot {
        let mut rows: Vec<UserFundingRow> = (1..=29)
            .map(|day| {
                payment(
                    day * 24 * 60 + 30,
                    "xyz:SILVER",
                    "212.54",
                    "-1.94",
                    "0.000021",
                    24,
                )
            })
            .collect();
        rows.push(payment(30, "xyz:SILVER", "212.54", "-2.05", "0.000021", 24));
        rows.push(payment(5 * 24 * 60, "BTC", "-0.5", "0.20", "0.00001", 1));
        rows.push(payment(
            20,
            "xyz:COPPER",
            "871.62",
            "0.0436",
            "-0.000009",
            1,
        ));
        let mut snapshot = HistorySnapshot::default();
        snapshot.funding.data = Some(Paged {
            rows,
            from_ms: Some(0),
            stopped_at: None,
        });
        snapshot
    }

    fn ledger_row(minutes_ago: u64, hash: &str, delta: LedgerDelta) -> LedgerRow {
        LedgerRow {
            time: now_ms() - minutes_ago * MINUTE_MS,
            hash: hash.into(),
            delta,
        }
    }

    fn the_transfers() -> HistorySnapshot {
        let mut snapshot = HistorySnapshot::default();
        snapshot.ledger.data = Some(Paged {
            rows: vec![
                ledger_row(
                    3 * 24 * 60,
                    "0x5c1e00000000000000000000000000000000000000000000000000000000009a04",
                    LedgerDelta::Deposit {
                        usdc: d("18706.66"),
                    },
                ),
                ledger_row(
                    3 * 24 * 60 + 2,
                    "0x81ad000000000000000000000000000000000000000000000000000000002f6c",
                    LedgerDelta::Deposit { usdc: d("19.80") },
                ),
                ledger_row(
                    60,
                    "0x77",
                    LedgerDelta::AccountClassTransfer {
                        usdc: d("500"),
                        to_perp: false,
                    },
                ),
            ],
            from_ms: Some(0),
            stopped_at: None,
        });
        snapshot
    }

    #[gpui_kit::test]
    fn history_is_a_section_on_trades_with_the_footer_and_no_trading_control(
        cx: &mut TestAppContext,
    ) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            assert_eq!(desk.read(cx).section(), Some(Section::History));
            assert_eq!(history.read(cx).tab(), Tab::Trades);
            assert_eq!(history.read(cx).period(), Period::Month);
            assert!(window.try_find("account-footer").is_some());
            for forbidden in [
                "Buy", "Sell", "Close", "Cancel", "Amend", "Deposit", "Withdraw", "Transfer",
            ] {
                assert!(
                    window.find_all_by_label(forbidden).is_empty(),
                    "a `{forbidden}` control is in History"
                );
            }
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn history_is_not_offered_before_connecting(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, false);
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(desk.read(cx).history_view().is_none());
            assert!(window.find_all_by_label("History").is_empty());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn a_tabs_filter_is_kept_across_a_round_trip_to_markets(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open_with(cx, Some(&["BTC"]));
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, _, cx| {
            history.update(cx, |view, cx| {
                view.set_snapshot(the_orders(), cx);
                view.set_tab(Tab::Orders, cx);
                view.order_filter_mut().status = Some(Status::Filled);
                view.refilter(cx);
            });
        })
        .unwrap();
        go_to_markets(window, &desk, cx);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, _, cx| {
            let view = history.read(cx);
            assert_eq!(view.tab(), Tab::Orders);
            assert_eq!(view.column(10, cx), ["Filled"]);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn the_counts_beside_the_tabs_follow_the_period(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            give_account(&desk, None, Some(walked(the_fills())), cx);
            history.update(cx, |view, cx| {
                view.set_grouped(false, cx);
                view.set_snapshot(the_orders(), cx);
            });
            window.render_frame(cx);
            assert_eq!(history.read(cx).count(Tab::Trades), Some(8));
            assert_eq!(history.read(cx).count(Tab::Orders), Some(3));
            history.update(cx, |view, cx| view.set_period(Period::Day, cx));
            // The NVDA fills were three days ago.
            assert_eq!(history.read(cx).count(Tab::Trades), Some(3));
            window.render_frame(cx);
            assert!(!window.find_all_by_label("Trades 3").is_empty());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn an_export_writes_the_listed_rows_and_nothing_else(cx: &mut TestAppContext) {
        let (window, desk, dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        let path = dir.path().join("ant.csv");
        cx.update_window(window.into(), |_, _, cx| {
            give_account(&desk, None, Some(walked(the_fills())), cx);
            history.update(cx, |view, cx| {
                view.set_grouped(false, cx);
                view.trade_filter_mut().coin = Some("xyz:ANT".into());
                view.refilter(cx);
                view.export_to(path.clone(), cx);
            });
            assert_eq!(
                history.read(cx).note().as_deref(),
                Some("Exported 3 rows to ant.csv.")
            );
        })
        .unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 4);
        assert!(lines[0].starts_with("time_utc,market,direction"));
        assert!(lines[1..].iter().all(|l| l.contains(",xyz:ANT,")));
    }

    #[gpui_kit::test]
    fn trades_show_each_fill_and_group_an_orders_fills(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, _, cx| {
            give_account(&desk, None, Some(walked(the_fills())), cx);
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let view = history.read(cx);
            // Grouped by default: the five NVDA fills are one row.
            assert_eq!(
                view.column(2, cx),
                ["Open Short", "Close Short", "Open Short", "Open Long"]
            );
            assert_eq!(view.column(4, cx)[3], "19.984 NVDA 5 fills");
            assert_eq!(view.column(3, cx)[3], "150.12");
            // The opening fill: its value and fee, and no closed PNL.
            assert_eq!(view.column(1, cx)[0], "ANT xyz");
            assert_eq!(view.column(4, cx)[0], "28.57 ANT");
            assert_eq!(view.column(5, cx)[0], "$2,999.85");
            assert_eq!(view.column(6, cx)[0], "$0.09");
            assert_eq!(view.column(8, cx)[0], "\u{2014}");
            assert_eq!(view.column(7, cx)[0], "Maker");
            // The closing fill realised PNL; the last ANT fill took.
            assert_eq!(view.column(8, cx)[1], "+$21.24");
            assert_eq!(view.column(7, cx)[2], "Taker");
            let totals = view.trade_totals();
            assert_eq!(totals.rows, 4);
            assert_eq!(totals.fills, 8);
            assert_eq!(totals.closed_pnl, d("21.24"));
            // Every fill, with grouping off.
            history.update(cx, |view, cx| view.set_grouped(false, cx));
            assert_eq!(history.read(cx).column(2, cx).len(), 8);
            // The venue held every fill: no note about how far back.
            assert!(window.try_find("history-reach").is_none());
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn trades_say_from_when_they_are_shown_when_the_venue_kept_no_older(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, _, cx| {
            give_account(
                &desk,
                None,
                Some(Fills {
                    rows: the_fills(),
                    capped: true,
                    walked: true,
                }),
                cx,
            );
        })
        .unwrap();
        cx.run_until_parked();
        cx.update_window(window.into(), |_, window, cx| {
            // Within 24 hours the fills reach back far enough.
            history.update(cx, |view, cx| view.set_period(Period::Day, cx));
            window.render_frame(cx);
            assert!(window.try_find("history-reach").is_none());
            history.update(cx, |view, cx| view.set_period(Period::Month, cx));
            window.render_frame(cx);
            assert!(window.try_find("history-reach").is_some());
            let note = history.update(cx, |view, cx| view.reach_text(cx)).unwrap();
            assert!(note.starts_with("* Trades before"), "{note}");
            assert!(note.contains("latest 10,000 fills"), "{note}");
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn orders_show_each_order_once_at_its_latest_status(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            history.update(cx, |view, cx| {
                view.set_snapshot(the_orders(), cx);
                view.set_tab(Tab::Orders, cx);
            });
            window.render_frame(cx);
            let view = history.read(cx);
            // Newest placed first; the order that rested and filled is one row.
            assert_eq!(view.column(10, cx), ["Filled", "Cancelled", "Open"]);
            assert_eq!(view.column(4, cx)[0], "28.57 ANT");
            assert_eq!(view.column(5, cx)[0], "28.57");
            assert_eq!(view.column(7, cx)[0], "$2,999.85");
            // The take-profit: at market, reduce-only, with its trigger.
            assert_eq!(view.column(2, cx)[1], "Close Short");
            assert_eq!(view.column(3, cx)[1], "Take Profit Market");
            assert_eq!(view.column(4, cx)[1], "Position");
            assert_eq!(view.column(6, cx)[1], "Market");
            assert_eq!(view.column(8, cx)[1], "Yes");
            assert_eq!(view.column(9, cx)[1], "Price below 85");
            // Its status says why it was cancelled.
            assert!(view.tip(1, 10, cx).unwrap().contains("reduce-only"));
            assert_eq!(view.tip(0, 10, cx), None);
            let (all, by) = view.order_counts();
            assert_eq!(all, 3);
            assert_eq!(by[&Status::Open], 1);
            // Only open orders.
            history.update(cx, |view, cx| {
                view.order_filter_mut().status = Some(Status::Open);
                view.refilter(cx);
            });
            assert_eq!(history.read(cx).column(10, cx), ["Open"]);
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn funding_payments_are_summed_by_market_and_listed_one_by_one(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            give_account(
                &desk,
                Some(vec![PositionRow {
                    size: d("212.54"),
                    ..position("SILVER", "xyz", 9_655)
                }]),
                None,
                cx,
            );
            history.update(cx, |view, cx| {
                view.set_snapshot(the_payments(), cx);
                view.set_tab(Tab::Funding, cx);
            });
            window.render_frame(cx);
            let view = history.read(cx);
            // The largest net cost first: SILVER, paid every hour for 30 days.
            assert_eq!(view.column(0, cx), ["SILVER xyz", "COPPER xyz", "BTC"]);
            assert_eq!(view.column(3, cx)[0], "720");
            assert_eq!(view.column(4, cx)[0], "-$58.31");
            assert_eq!(view.column(5, cx)[0], "\u{2014}");
            assert_eq!(view.column(6, cx)[0], "-$58.31");
            assert_eq!(view.column(2, cx)[0], "212.54 SILVER");
            // BTC is no longer held.
            assert_eq!(view.column(2, cx)[2], "\u{2014}");
            assert_eq!(view.column(1, cx)[2], "Short");
            let totals = view.funding_totals();
            assert_eq!(totals.received, d("0.2436"));
            assert_eq!(totals.net, d("-58.31") + d("0.2436"));
            // Every payment: the newest is COPPER's, received.
            history.update(cx, |view, cx| view.set_every_payment(true, cx));
            let view = history.read(cx);
            assert_eq!(view.column(1, cx)[0], "COPPER xyz");
            assert_eq!(view.column(4, cx)[0], "+$0.0436");
            assert_eq!(view.column(5, cx)[0], "-0.0009%");
            // An older day the venue summed shows the day and its count.
            assert!(
                view.column(0, cx)[1].ends_with("24 payments"),
                "{}",
                view.column(0, cx)[1]
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn deposits_and_transfers_are_listed_and_a_hash_can_be_copied(cx: &mut TestAppContext) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            history.update(cx, |view, cx| {
                view.set_snapshot(the_transfers(), cx);
                view.set_tab(Tab::Transfers, cx);
            });
            window.render_frame(cx);
            let view = history.read(cx);
            assert_eq!(view.column(1, cx), ["Transfer", "Deposit", "Deposit"]);
            // A transfer to spot: no change in account value.
            assert_eq!(
                (
                    view.column(3, cx)[0].as_str(),
                    view.column(4, cx)[0].as_str()
                ),
                ("Perpetuals", "Spot")
            );
            assert_eq!(view.column(5, cx)[0], "\u{2014}");
            // A deposit: no fee reported.
            assert_eq!(
                (
                    view.column(3, cx)[1].as_str(),
                    view.column(4, cx)[1].as_str()
                ),
                ("Arbitrum", "Perpetuals")
            );
            assert_eq!(view.column(5, cx)[1], "+$18,706.66");
            assert_eq!(view.column(6, cx)[1], "\u{2014}");
            assert_eq!(view.column(7, cx)[1], "0x5c1e\u{2026}9a04");
            let totals = view.ledger_totals();
            assert_eq!(totals.deposited, d("18726.46"));
            assert_eq!(totals.withdrawn, Decimal::ZERO);
            assert_eq!(totals.fees, Decimal::ZERO);
            // Copy the deposit's hash.
            history.update(cx, |view, cx| view.press_for_test("copy-hash", 1, cx));
            assert_eq!(
                cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
                Some("0x5c1e00000000000000000000000000000000000000000000000000000000009a04")
            );
            assert_eq!(
                history.read(cx).note().as_deref(),
                Some("Copied the transaction hash.")
            );
        })
        .unwrap();
    }

    #[gpui_kit::test]
    fn a_tab_shows_placeholders_then_rows_and_says_when_it_is_empty_or_stale(
        cx: &mut TestAppContext,
    ) {
        let (window, desk, _dir) = open(cx, true);
        let history = go_to_history(window, &desk, cx);
        cx.update_window(window.into(), |_, window, cx| {
            history.update(cx, |view, cx| view.set_tab(Tab::Orders, cx));
            window.render_frame(cx);
            // Nothing yet: placeholders, never zeros.
            assert!(window.try_find("history-loading").is_some());

            // Nothing in the period.
            let mut empty = the_transfers();
            if let Some(ledger) = &mut empty.ledger.data {
                for row in &mut ledger.rows {
                    row.time = now_ms() - 40 * DAY_MS;
                }
            }
            history.update(cx, |view, cx| {
                view.set_snapshot(empty, cx);
                view.set_tab(Tab::Transfers, cx);
            });
            window.render_frame(cx);
            assert!(history.read(cx).column(1, cx).is_empty());
            assert_eq!(
                history.read(cx).empty_text(cx),
                "No deposits or withdrawals in the last 30 days."
            );

            // Orders held and a refresh that failed: still shown, said to be
            // not current. Funding never loaded and failed: only it says so.
            let mut snapshot = the_orders();
            snapshot.orders.error = Some(VenueError::Status(500));
            snapshot.orders.fetched_at = Some(std::time::SystemTime::now());
            snapshot.funding.error = Some(VenueError::Status(502));
            history.update(cx, |view, cx| {
                view.set_snapshot(snapshot, cx);
                view.set_tab(Tab::Orders, cx);
            });
            window.render_frame(cx);
            assert!(window.try_find("history-stale").is_some());
            assert!(window.try_find("history-failed").is_none());
            assert_eq!(history.read(cx).column(10, cx).len(), 3);
            history.update(cx, |view, cx| view.set_tab(Tab::Funding, cx));
            window.render_frame(cx);
            assert!(window.try_find("history-failed").is_some());
            assert!(window.try_find("history-stale").is_none());
        })
        .unwrap();
    }
}

// ---------------------------------------------------------------------------
// The Markets position panel opens the Portfolio's own dialogs.

#[gpui_kit::test]
fn the_position_panel_opens_the_same_close_dialog_as_the_positions_tab(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);

    press_position(cx, window, &view, "close-market", 110_000);
    let from_tab = cx
        .update_window(window.into(), |_, _, cx| view.read(cx).close_state(cx))
        .unwrap();
    click(cx, window, "dialog-keep");

    let nvda = MarketId::from_parts("xyz", "NVDA");
    on_view(cx, window, &view, |v, w, cx| v.open_close(&nvda, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert_eq!(view.open_dialog(), Some("close-market"));
        assert_eq!(view.close_state(cx), from_tab);
        // The whole position to start with.
        assert_eq!(view.close_state(cx).unwrap().1, "10");
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_position_panel_opens_tpsl_and_cancel_as_the_tabs_do(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let orders = vec![
        level(501, Tpsl::Tp, "160"),
        a_limit(1, "NVDA", Side::Buy, "12", "154.20"),
    ];
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], orders);
    let nvda = MarketId::from_parts("xyz", "NVDA");

    on_view(cx, window, &view, |v, w, cx| v.open_tpsl(&nvda, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("tpsl"));
    })
    .unwrap();
    click(cx, window, "dialog-keep");

    on_view(cx, window, &view, |v, w, cx| v.open_cancel(1, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("cancel"));
    })
    .unwrap();
    // Nothing is sent until confirmed.
    assert!(staged.cancels.borrow().is_empty());
    assert!(staged.closes.borrow().is_empty());

    // A market the account holds nothing in opens nothing.
    click(cx, window, "dialog-keep");
    let btc = MarketId::new("BTC");
    on_view(cx, window, &view, |v, w, cx| v.open_close(&btc, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
}

#[gpui_kit::test]
fn cancelling_a_markets_orders_leaves_other_markets_alone(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    staged
        .answers
        .borrow_mut()
        .push_back(Ok(Answer::Each(vec![Outcome::DONE, Outcome::DONE])));
    let mut orders = three_orders();
    orders.push(a_stop(4));
    let (window, _desk, view, _dir) = orders_tab(cx, &staged, orders);
    let nvda = MarketId::new("NVDA");

    cx.update_window(window.into(), |_, _, cx| {
        let oids: Vec<u64> = view
            .read(cx)
            .market_orders(&nvda)
            .iter()
            .map(|o| o.oid)
            .collect();
        assert_eq!(oids, [1, 4]);
    })
    .unwrap();

    on_view(cx, window, &view, |v, w, cx| {
        v.open_cancel_market(&nvda, w, cx)
    });
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(view.read(cx).open_dialog(), Some("cancel-market"));
        assert!(window.try_find("cancel-all-dialog").is_some());
        assert_eq!(
            view.read(cx).cancel_all_heading().as_deref(),
            Some("Cancel all 2 open NVDA orders?")
        );
    })
    .unwrap();
    assert!(staged.cancels.borrow().is_empty());

    click(cx, window, "dialog-confirm");
    assert_eq!(
        *staged.cancels.borrow(),
        [vec![
            Target {
                asset: 110_000,
                oid: 1
            },
            Target {
                asset: 110_000,
                oid: 4
            },
        ]]
    );

    // A market with no orders cannot be cancelled.
    let eth = MarketId::new("ETH");
    on_view(cx, window, &view, |v, w, cx| {
        v.open_cancel_market(&eth, w, cx)
    });
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_mainnet_trading_off_the_panel_is_told_why_and_opens_nothing(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Err(Unavailable::MainnetOff)));
    let (window, _desk, view, _dir) = positions_tab(cx, &staged, vec![long_nvda()], vec![]);
    cx.update_window(window.into(), |_, _, cx| {
        let view = view.read(cx);
        assert!(!view.can_act_now());
        let (heading, sentence) = view.actions_blocked(true).unwrap();
        assert_eq!(heading, "Trading on mainnet is switched off");
        assert!(sentence.contains("Allow trading on mainnet"), "{sentence}");
    })
    .unwrap();
    let nvda = MarketId::from_parts("xyz", "NVDA");
    on_view(cx, window, &view, |v, w, cx| v.open_close(&nvda, w, cx));
    on_view(cx, window, &view, |v, w, cx| v.open_tpsl(&nvda, w, cx));
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(view.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert!(staged.closes.borrow().is_empty());
}

// ---------------------------------------------------------------------------
// The Markets position panel.

/// ETH as an action names it, for the panel's tests.
const ETH_ASSET: Asset = Asset {
    number: 1,
    sz_decimals: 4,
    spot: false,
};

/// 13.6429 ETH long at 5x cross, as the spec's examples have it.
fn eth_long_row() -> PositionRow {
    PositionRow {
        asset: Some(ETH_ASSET),
        market: "ETH".into(),
        dex: String::new(),
        leverage: 5,
        mode: MarginMode::Cross,
        size: dec("13.6429"),
        value: dec("23715.42"),
        entry: dec("1733.1"),
        mark: Some(dec("1738.3")),
        pnl: dec("70.94"),
        roe: dec("0.015"),
        liquidation: Some(dec("1412.6")),
        margin: dec("4743.08"),
        funding: dec("-18.42"),
    }
}

fn eth_order(
    oid: u64,
    kind: &str,
    tpsl: Option<Tpsl>,
    side: Side,
    size: &str,
    at: &str,
) -> OrderRow {
    OrderRow {
        market: "ETH".into(),
        dex: String::new(),
        kind: kind.into(),
        side,
        remaining: dec(size),
        original: dec(size),
        price: dec(at),
        trigger_px: tpsl.map(|_| dec(at)),
        tpsl,
        amendable: tpsl.is_none(),
        asset: Some(ETH_ASSET),
        ..a_limit(oid, "ETH", side, size, at)
    }
}

fn eth_orders() -> Vec<OrderRow> {
    vec![
        eth_order(
            11,
            "Take Profit Market",
            Some(Tpsl::Tp),
            Side::Sell,
            "0",
            "1900",
        ),
        eth_order(12, "Stop Market", Some(Tpsl::Sl), Side::Sell, "0", "1690"),
        eth_order(13, "Limit", None, Side::Buy, "2", "1720"),
    ]
}

fn eth_fill(ago_ms: u64, side: FillSide, size: &str, at: &str, realised: &str, fee: &str) -> Fill {
    let time_ms = now_ms() - ago_ms;
    Fill {
        coin: "ETH".into(),
        side,
        price: dec(at),
        size: dec(size),
        time_ms,
        realised: dec(realised),
        fee: dec(fee),
        id: time_ms,
        ..Fill::blank()
    }
}

/// Markets on ETH with the account's position, orders and fills as given,
/// and this stand-in for the gate.
fn eth_panel(
    cx: &mut TestAppContext,
    staged: &Rc<Staged>,
    positions: Option<Vec<PositionRow>>,
    orders: Vec<OrderRow>,
    fills: Option<Fills>,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<Markets>,
    Entity<Portfolio>,
    tempfile::TempDir,
) {
    let (window, desk, dir) = open_with(cx, Some(&["ETH", "BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    let portfolio = cx
        .update_window(window.into(), |_, window, cx| {
            let portfolio = desk.read(cx).portfolio_view().unwrap();
            portfolio.update(cx, |p, cx| {
                p.use_trading(staged.clone());
                p.set_orders(orders, cx);
                if let Some(positions) = positions {
                    p.set_positions(positions, cx);
                }
                if let Some(fills) = fills {
                    p.set_fills(fills, cx);
                }
            });
            markets.update(cx, |view, cx| {
                view.set_snapshot(charted("ETH", &["ETH", "BTC"], 40), cx);
                view.focus_market("ETH", window, cx);
            });
            window.render_frame(cx);
            portfolio
        })
        .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
    (window, desk, markets, portfolio, dir)
}

#[gpui_kit::test]
fn the_position_panel_states_the_position_as_the_spec_does(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, _markets, _portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![eth_long_row()]), vec![], None);
    cx.update_window(window.into(), |_, window, _| {
        for label in [
            "LONG \u{b7} 5x cross",
            "size +13.6429 ETH, $23,715",
            "unrealised +$70.94, +1.50% ROE",
            "Entry price 1,733.1",
            "Mark price 1,738.3",
            "Liquidation price 1,412.6",
            "Margin $4,743 \u{b7} 5x",
            "Funding since open -$18.42",
        ] {
            assert!(
                window.try_find_by_label(label).is_some(),
                "missing `{label}`"
            );
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn before_positions_arrive_the_panel_shows_placeholders_not_flat(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, markets, _portfolio, _dir) = eth_panel(cx, &staged, None, vec![], None);
    cx.update_window(window.into(), |_, window, cx| {
        let model = markets.read(cx).position_panel(cx).unwrap();
        assert_eq!(model.holding, market_position::Holding::Loading);
        assert!(window.try_find("position-side").is_none());
        assert!(window.try_find_by_label("Flat").is_none());
        assert!(window.try_find("position-flat").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn flat_says_so_and_offers_no_position_actions(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let (window, _desk, _markets, _portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![]), vec![], None);
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find_by_label("Flat").is_some());
        assert!(window.try_find("position-flat").is_some());
        assert!(window.try_find("position-close").is_none());
        assert!(window.try_find("position-tpsl").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_pnl_breakdown_starts_collapsed_opens_from_the_keyboard_and_stays_open(
    cx: &mut TestAppContext,
) {
    let staged = Rc::new(Staged::new(Ok(())));
    let fills = walked(vec![eth_fill(
        3_600_000,
        FillSide::Sold,
        "1",
        "1740",
        "-180",
        "6.2",
    )]);
    let (window, _desk, markets, _portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![eth_long_row()]), vec![], Some(fills));
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(
            window.find_by_label("PNL breakdown").expanded(),
            Some(false)
        );
        assert!(window.try_find("pnl-rows").is_none());
        // The header carries the day's total: -186.20 realised + 70.94 open.
        let model = markets.read(cx).position_panel(cx).unwrap();
        assert_eq!(model.day_total(), Some(("-$115".into(), -1)));

        markets.update(cx, |view, cx| view.focus_pnl_header(window, cx));
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(window.find_by_label("PNL breakdown").expanded(), Some(true));
        assert!(window.try_find("pnl-rows").is_some());
        assert!(
            window
                .try_find_by_label("24 hours: realised -$186, total -$115")
                .is_some()
        );

        // Another market keeps it open.
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("BTC"));
        assert_eq!(window.find_by_label("PNL breakdown").expanded(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_last_trade_is_shown_or_said_to_be_none(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let fills = walked(vec![
        eth_fill(3_600_000, FillSide::Sold, "1", "1700", "0", "1"),
        eth_fill(60_000, FillSide::Bought, "2.5", "1741.2", "0", "2.18"),
    ]);
    let (window, _desk, markets, _portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![eth_long_row()]), vec![], Some(fills));
    cx.update_window(window.into(), |_, window, cx| {
        let trade = markets
            .read(cx)
            .position_panel(cx)
            .unwrap()
            .trade
            .unwrap()
            .unwrap();
        let label = format!("{} Buy 2.5 at 1,741.2, fee $2.18", trade.time);
        assert!(window.try_find_by_label(&label).is_some(), "{label}");

        // BTC has no fill among those held.
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert!(window.try_find_by_label("No recent trade").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn open_orders_summarise_when_collapsed_and_list_and_cancel_when_open(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    // Flat, so the open list fits without scrolling: the test cannot scroll.
    let (window, _desk, _markets, portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![]), eth_orders(), None);
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find_by_label("Open orders").expanded(), Some(false));
        assert!(window.try_find_by_label("3 open orders").is_some());
        assert!(
            window
                .try_find_by_label("TP \u{b7} SL \u{b7} 1 limit")
                .is_some()
        );
        assert!(window.try_find("orders-list").is_none());
    })
    .unwrap();

    click(cx, window, "orders-header");
    cx.update_window(window.into(), |_, window, _| {
        assert_eq!(window.find_by_label("Open orders").expanded(), Some(true));
        assert!(window.try_find("orders-list").is_some());
        assert!(
            window
                .try_find_by_label("Cancel Sell Take profit order in ETH")
                .is_some()
        );
        assert!(
            window
                .try_find_by_label("Cancel Buy Limit order in ETH")
                .is_some()
        );
    })
    .unwrap();

    cx.update_window(window.into(), |_, window, cx| {
        window.click(("cancel-market-order", 13usize), cx)
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(portfolio.read(cx).open_dialog(), Some("cancel"));
    })
    .unwrap();
    click(cx, window, "dialog-keep");

    click(cx, window, "cancel-market-orders");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(portfolio.read(cx).open_dialog(), Some("cancel-market"));
        assert_eq!(
            portfolio.read(cx).cancel_all_heading().as_deref(),
            Some("Cancel all 3 open ETH orders?")
        );
    })
    .unwrap();
    click(cx, window, "dialog-keep");
    assert!(staged.cancels.borrow().is_empty());
}

#[gpui_kit::test]
fn guarded_actions_are_shown_but_cannot_be_used_and_say_why(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Err(Unavailable::MainnetOff)));
    let (window, _desk, _markets, portfolio, _dir) =
        eth_panel(cx, &staged, Some(vec![eth_long_row()]), eth_orders(), None);
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find("position-close").is_some());
        assert!(window.try_find("position-guard").is_some());
        // The one-line notice leaves the collapsed open orders in view.
        let orders = window.find("orders-header").bounds();
        let guard = window.find("position-guard").bounds();
        assert!(orders.bottom() <= guard.top(), "{orders:?} {guard:?}");
        assert!(
            window
                .find("position-guard")
                .label()
                .is_some_and(|l| l.contains("Allow trading on mainnet")),
        );
    })
    .unwrap();
    click(cx, window, "position-close");
    click(cx, window, "position-tpsl");
    cx.update_window(window.into(), |_, _, cx| {
        assert_eq!(portfolio.read(cx).open_dialog(), None);
    })
    .unwrap();
    assert!(staged.closes.borrow().is_empty());
}

#[gpui_kit::test]
fn expanding_both_sections_keeps_the_panels_height_and_scrolls_within(cx: &mut TestAppContext) {
    let staged = Rc::new(Staged::new(Ok(())));
    let fills = walked(vec![eth_fill(
        60_000,
        FillSide::Bought,
        "2.5",
        "1741.2",
        "0",
        "2.18",
    )]);
    let (window, _desk, _markets, _portfolio, _dir) = eth_panel(
        cx,
        &staged,
        Some(vec![eth_long_row()]),
        eth_orders(),
        Some(fills),
    );
    let height = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, _| {
            window.find("market-panel").bounds().size.height
        })
        .unwrap()
    };
    let before = height(cx);
    // Collapsed, everything down to Open orders fits without scrolling.
    cx.update_window(window.into(), |_, window, _| {
        let panel = window.find("market-panel").bounds();
        let orders = window.find("orders-header").bounds();
        assert!(orders.bottom() <= panel.bottom(), "{orders:?} {panel:?}");
    })
    .unwrap();
    // Open orders first: the PNL breakdown, once open, pushes it down.
    click(cx, window, "orders-header");
    click(cx, window, "pnl-header");
    assert_eq!(height(cx), before);
    cx.update_window(window.into(), |_, window, _| {
        let panel = window.find("market-panel").bounds();
        let column = window.find("position-panel").bounds();
        assert!(column.bottom() <= panel.bottom());
        assert!(window.try_find("pnl-rows").is_some());
        assert!(window.try_find("orders-list").is_some());
    })
    .unwrap();
    assert_eq!(before, px(406.));
}

/// Markets on BTC, charted from 99 to 141, holding this position or none.
fn btc_chart_with(
    cx: &mut TestAppContext,
    held: Option<PositionRow>,
) -> (WindowHandle<Root>, Entity<Markets>, tempfile::TempDir) {
    let (window, desk, dir) = open_with(cx, Some(&["BTC"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC"], 40), cx)
        });
        give_account(&desk, Some(held.into_iter().collect()), None, cx);
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
    })
    .unwrap();
    (window, markets, dir)
}

#[gpui_kit::test]
fn the_entry_price_is_a_line_on_the_chart_while_it_is_in_range(cx: &mut TestAppContext) {
    let held = PositionRow {
        size: dec("0.5"),
        entry: dec("120"),
        ..position("BTC", "", 60)
    };
    let (window, _markets, _dir) = btc_chart_with(cx, Some(held));
    cx.update_window(window.into(), |_, window, _| {
        let line = window.find("entry-line");
        assert_eq!(line.label(), Some("Entry 120 \u{b7} +0.5"));
        // Between the plot's top and bottom, about halfway (120 of 99..141).
        let plot = window.find("chart-plot").bounds();
        let y = line.bounds().top();
        assert!(y > plot.top() && y < plot.bottom(), "{y:?} {plot:?}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_entry_outside_the_candles_draws_nothing_and_leaves_the_axis_alone(cx: &mut TestAppContext) {
    let (_w, flat, _d) = btc_chart_with(cx, None);
    let flat_range = cx.update(|cx| flat.read(cx).chart_range(cx));
    assert!(flat_range.is_some());

    let held = PositionRow {
        size: dec("-0.5"),
        entry: dec("50"),
        ..position("BTC", "", 60)
    };
    let (window, markets, _dir) = btc_chart_with(cx, Some(held));
    cx.update_window(window.into(), |_, window, cx| {
        assert!(window.try_find("entry-line").is_none());
        assert_eq!(markets.read(cx).chart_range(cx), flat_range);
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_no_position_there_is_no_entry_line(cx: &mut TestAppContext) {
    let (window, _markets, _dir) = btc_chart_with(cx, None);
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find("entry-line").is_none());
    })
    .unwrap();
}

// ---- Charts of their own -------------------------------------------------

#[gpui_kit::test]
fn two_charts_of_one_market_and_period_share_one_hold_and_closing_both_lets_it_go(
    cx: &mut TestAppContext,
) {
    use crate::markets::{ChartPanel, MarketSource};
    use galata_desk_core::market::MarketPeriod;
    use galata_desk_core::market::feed::ChartKey;

    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    let model = cx.update(|cx| markets.read(cx).model());
    let eth = || MarketSource::Own(Some(MarketId::new("ETH")));
    let key = |period| ChartKey {
        market: MarketId::new("ETH"),
        period,
    };
    // The windowed chart follows the selection and holds nothing.
    cx.update(|cx| assert!(model.read(cx).chart_holds().is_empty()));

    let first = cx.new(|cx| ChartPanel::new(model.clone(), eth(), cx));
    let second = cx.new(|cx| ChartPanel::new(model.clone(), eth(), cx));
    cx.update(|cx| assert_eq!(model.read(cx).chart_holds(), [(key(MarketPeriod::Day), 2)]));

    // A chart that moves to another period holds that one instead.
    second.update(cx, |chart, cx| {
        chart.set_period(Some(MarketPeriod::Week), cx)
    });
    cx.update(|cx| {
        assert_eq!(
            model.read(cx).chart_holds(),
            [(key(MarketPeriod::Day), 1), (key(MarketPeriod::Week), 1)]
        )
    });

    // Closing both lets both go.
    drop(first);
    drop(second);
    // Dropped entities are released when the app next flushes its effects.
    cx.update(|_| {});
    cx.update(|cx| assert!(model.read(cx).chart_holds().is_empty()));
}

// ---- Dock panels ---------------------------------------------------------

/// The records of every Markets panel in a saved layout, by key.
fn saved_records(
    state: &gpui_kit::component::dock::PanelState,
) -> Vec<crate::markets::dock::PanelRecord> {
    let mut records = Vec::new();
    if let Some(record) = crate::markets::dock::PanelRecord::from_state(state) {
        records.push(record);
    }
    for child in &state.children {
        records.extend(saved_records(child));
    }
    records.sort_by(|a, b| a.key.cmp(&b.key));
    records
}

#[gpui_kit::test]
fn each_panel_kind_comes_back_from_a_saved_layout_with_its_market_and_period(
    cx: &mut TestAppContext,
) {
    use crate::markets::dock::{self, PanelKind, PanelRecord, Role, SavedMarket};
    use crate::markets::{ChartPanel, DetailPanel, PositionPanel};
    use galata_desk_core::market::MarketPeriod;
    use gpui_kit::component::dock::{DockLayout, DockSkin};

    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH", "xyz:NVDA"]));
    let markets = go_to_markets(window, &desk, cx);
    let model = cx.update(|cx| markets.read(cx).model());

    cx.update_window(window.into(), |_, window, cx| {
        let new_area = |window: &mut gpui_kit::Window, cx: &mut gpui_kit::App| {
            let (area, _skin) = DockSkin::dock_area("markets-workspace", Some(1), window, cx);
            dock::bind(&area, &model, cx);
            area
        };
        let fresh = |kind, market: Option<&str>, interval: Option<&str>| PanelRecord {
            key: format!("{kind:?}"),
            kind,
            market: market.map(|m| SavedMarket::from(&MarketId::new(m))),
            interval: interval.map(str::to_string),
            tab: None,
            roles: [Role::Listen, Role::Off, Role::Both],
            actions: vec![],
        };
        let chart = fresh(PanelKind::Chart, Some("ETH"), Some("7d"));
        let tiles = fresh(PanelKind::Tiles, Some("BTC"), None);
        let detail = fresh(PanelKind::Detail, Some("xyz:NVDA"), None);
        let position = fresh(PanelKind::Position, Some("BTC"), None);

        let area = new_area(window, cx);
        let mut layout = DockLayout::h_split();
        for record in [&chart, &tiles, &detail, &position] {
            let panel = dock::new_panel(record.kind, model.clone(), record, window, cx);
            layout = layout.child(DockLayout::tabs().panel_view(panel, cx), None);
        }
        area.update(cx, |area, cx| area.set_center(layout, window, cx));
        let saved = area.read(cx).dump(cx);
        assert_eq!(
            saved_records(&saved.center),
            {
                let mut all = vec![
                    chart.clone(),
                    tiles.clone(),
                    detail.clone(),
                    position.clone(),
                ];
                all.sort_by(|a, b| a.key.cmp(&b.key));
                all
            },
            "each panel saves its kind, market, period and links"
        );

        // Loaded into a new area, each comes back as it was.
        let restored = new_area(window, cx);
        restored
            .update(cx, |area, cx| area.load(saved.clone(), window, cx))
            .unwrap();
        let again = restored.read(cx).dump(cx);
        assert_eq!(saved_records(&again.center), saved_records(&saved.center));

        // And they are the real panels, on the same markets.
        let views: Vec<_> = {
            let area = restored.read(cx);
            area.layout(gpui_kit::component::dock::DockPlacement::Center)
                .unwrap()
                .panels()
                .filter_map(|id| area.panel(id).cloned())
                .collect()
        };
        assert_eq!(views.len(), 4);
        let mut titles: Vec<String> = Vec::new();
        for view in views {
            let view = view.view();
            if let Ok(chart) = view.clone().downcast::<ChartPanel>() {
                let chart = chart.read(cx);
                assert_eq!(chart.market(cx).as_ref().map(MarketId::coin), Some("ETH"));
                assert_eq!(chart.period(cx), MarketPeriod::Week);
                titles.push(chart.title_text(cx));
            } else if let Ok(detail) = view.clone().downcast::<DetailPanel>() {
                titles.push(detail.read(cx).title_text(cx));
            } else if let Ok(position) = view.clone().downcast::<PositionPanel>() {
                titles.push(position.read(cx).title_text(cx));
            } else {
                let tiles = view
                    .downcast::<crate::markets::TilesPanel>()
                    .expect("the fourth is the tiles");
                assert_eq!(
                    tiles.read(cx).outlined(cx).as_ref().map(MarketId::coin),
                    Some("BTC")
                );
                titles.push(tiles.read(cx).title_text(cx));
            }
        }
        titles.sort();
        assert_eq!(
            titles,
            [
                "ETH \u{b7} 7D",
                "NVDA (xyz)",
                "Position \u{b7} BTC",
                "Watchlist"
            ]
        );
        // The restored chart holds its own candles in the feed.
        assert!(
            model
                .read(cx)
                .chart_holds()
                .iter()
                .any(|(key, _)| key.market.coin() == "ETH" && key.period == MarketPeriod::Week)
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_a_ticket_panel_buy_fills_and_focuses_it_and_opens_no_dialog(cx: &mut TestAppContext) {
    use crate::markets::TicketPanel;
    use crate::markets::dock::DockMeta;

    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    let portfolio = cx.update(|cx| desk.read(cx).portfolio_view().unwrap());

    // Without a ticket panel, Buy opens the ticket over the screen.
    cx.update_window(window.into(), |_, window, cx| {
        give_account(&desk, Some(vec![]), None, cx);
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        window.click("trade-buy", cx);
        window.render_frame(cx);
        assert_eq!(portfolio.read(cx).open_dialog(), Some("ticket"));
        assert!(window.try_find("ticket-dialog").is_some());
        portfolio.update(cx, |p, cx| p.dismiss_dialog(cx));
        window.render_frame(cx);
        assert_eq!(portfolio.read(cx).ticket_open(), None);
    })
    .unwrap();

    // With one docked, Buy fills it and puts the keyboard in it.
    let panel = cx.update(|cx| {
        let dock = DockMeta::fresh(cx);
        cx.new(|cx| TicketPanel::new(portfolio.clone(), dock, cx))
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("trade-buy", cx);
        window.render_frame(cx);
        let p = portfolio.read(cx);
        assert_eq!(p.open_dialog(), None, "no dialog over the screen");
        assert!(window.try_find("ticket-dialog").is_none());
        assert_eq!(p.ticket_open(), Some(("BTC".into(), true, true)));
        assert!(
            p.ticket_focused(window, cx),
            "the keyboard is in the ticket"
        );
    })
    .unwrap();

    // Sell on the same market keeps what was typed and changes side.
    cx.update_window(window.into(), |_, window, cx| {
        portfolio.update(cx, |p, cx| p.type_ticket("25", "", window, cx));
        window.click("trade-sell", cx);
        window.render_frame(cx);
        let p = portfolio.read(cx);
        assert_eq!(p.ticket_open(), Some(("BTC".into(), false, true)));
        assert_eq!(p.ticket_size_text(cx).as_deref(), Some("25"));
        assert_eq!(p.open_dialog(), None);
    })
    .unwrap();

    // Closing the panel takes its ticket; Buy then opens one over the screen.
    drop(panel);
    cx.update(|_| {});
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(portfolio.read(cx).ticket_open(), None);
        window.render_frame(cx);
        window.click("trade-buy", cx);
        window.render_frame(cx);
        assert_eq!(portfolio.read(cx).open_dialog(), Some("ticket"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_blotter_shows_the_sections_lists_and_closes_only_through_their_confirmation(
    cx: &mut TestAppContext,
) {
    use crate::markets::dock::DockMeta;
    use crate::markets::{BlotterPanel, BlotterTab};

    let staged = Rc::new(Staged::new(Ok(())));
    let (window, desk, portfolio, _dir) = positions_tab(
        cx,
        &staged,
        vec![long_nvda()],
        vec![level(7, Tpsl::Tp, "180")],
    );
    // Funding draws its table once shown; Markets is where the blotter lives.
    cx.update_window(window.into(), |_, window, cx| {
        desk.update(cx, |d, cx| d.show(Section::Funding, window, cx));
        window.render_frame(cx);
        desk.update(cx, |d, cx| d.show(Section::Markets, window, cx));
        window.render_frame(cx);
    })
    .unwrap();
    let markets = cx.update(|cx| desk.read(cx).markets_view().unwrap());
    let model = cx.update(|cx| markets.read(cx).model());
    let blotter = cx
        .update_window(window.into(), |_, window, cx| {
            let dock = DockMeta::fresh(cx);
            cx.new(|cx| BlotterPanel::new(model.clone(), BlotterTab::default(), dock, window, cx))
        })
        .unwrap();

    cx.update_window(window.into(), |_, _, cx| {
        let b = blotter.read(cx);
        assert_eq!(b.tab(), BlotterTab::Positions);
        let same = |tab: BlotterTab, source: &crate::grid::Grid| {
            let table = b.table(tab);
            let mirror = table.read(cx).delegate();
            assert_eq!(mirror.col_count(), source.col_count(), "{tab:?}");
            assert_eq!(mirror.empty_text(), source.empty_text(), "{tab:?}");
            for col in 0..source.col_count() {
                assert_eq!(
                    mirror.column_texts(col),
                    source.column_texts(col),
                    "{tab:?} column {col}"
                );
            }
        };
        let p = portfolio.read(cx);
        assert_eq!(p.positions_grid(cx).column_texts(0).len(), 1);
        same(BlotterTab::Positions, p.positions_grid(cx));
        assert_eq!(p.orders_grid(cx).column_texts(0).len(), 1);
        same(BlotterTab::Orders, p.orders_grid(cx));
        let history = desk.read(cx).history_view().unwrap();
        same(BlotterTab::Trades, history.read(cx).trades_grid(cx));

        // Funding: the same figures, row by row, as the section draws them.
        let drawn = desk.read(cx).funding_view().unwrap().read(cx).drawn();
        let table = b.table(BlotterTab::Funding);
        let mirror = table.read(cx).delegate();
        let rows: Vec<String> = (0..drawn.figures_by_row.len())
            .map(|row| {
                (1..8)
                    .map(|col| mirror.cell(row, col).unwrap().plain())
                    .collect::<Vec<_>>()
                    .join("|")
            })
            .collect();
        assert!(!rows.is_empty());
        assert_eq!(rows, drawn.figures_by_row);
    })
    .unwrap();

    // Close at market from the blotter opens Portfolio's confirmation, and
    // nothing is sent.
    cx.update_window(window.into(), |_, window, cx| {
        let table = blotter.read(cx).table(BlotterTab::Positions);
        let grid = table.read(cx).delegate().mirror();
        grid.press("close-market", 110_000, window, cx);
        window.render_frame(cx);
        assert_eq!(portfolio.read(cx).open_dialog(), Some("close-market"));
        assert!(staged.closes.borrow().is_empty());
    })
    .unwrap();
}

// ---- The fullscreen workspace --------------------------------------------

/// Draw Markets as fullscreen, or not, and draw the window.
fn fullscreen(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    markets: &Entity<Markets>,
    on: bool,
) {
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |m, cx| m.set_fullscreen(on, cx));
        window.render_frame(cx);
        // What the switch changed shows on the next frame.
        window.render_frame(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn entering_fullscreen_shows_the_workspace_and_leaving_shows_channel_as_market(
    cx: &mut TestAppContext,
) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH", "xyz:NVDA"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find("markets-fixed").is_some());
        assert!(window.try_find("markets-workspace").is_none());
    })
    .unwrap();

    // Entering fullscreen: the workspace, with its panels.
    fullscreen(cx, window, &markets, true);
    cx.update_window(window.into(), |_, window, cx| {
        assert!(window.try_find("markets-workspace").is_some());
        assert!(window.try_find("markets-fixed").is_none());
        let area = markets.read(cx).workspace_area().unwrap();
        let panels = area
            .read(cx)
            .layout(gpui_kit::component::dock::DockPlacement::Center)
            .unwrap()
            .panels()
            .count();
        assert_eq!(panels, 7, "the Trader layout's panels");
        assert!(window.try_find("blotter").is_some());
        assert!(window.try_find("ticket-panel").is_some());
    })
    .unwrap();

    // Leaving while channel A shows NVDA: the fixed layout, on NVDA.
    cx.update(|cx| markets.update(cx, |m, _| m.set_channel_a("xyz:NVDA")));
    fullscreen(cx, window, &markets, false);
    cx.update_window(window.into(), |_, window, cx| {
        assert!(window.try_find("markets-fixed").is_some());
        assert!(window.try_find("markets-workspace").is_none());
        assert_eq!(
            markets.read(cx).selected_market(cx).as_deref(),
            Some("xyz:NVDA")
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn coming_back_to_fullscreen_finds_the_panels_where_they_were(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    fullscreen(cx, window, &markets, true);
    // The user changes the layout: here, a chart closed.
    let before = cx
        .update_window(window.into(), |_, window, cx| {
            use gpui_kit::component::dock::DockPlacement;
            let area = markets.read(cx).workspace_area().unwrap();
            let count = |cx: &gpui_kit::App| {
                area.read(cx)
                    .layout(DockPlacement::Center)
                    .unwrap()
                    .panels()
                    .count()
            };
            let chart = {
                let a = area.read(cx);
                a.layout(DockPlacement::Center)
                    .unwrap()
                    .panels()
                    .find_map(|id| {
                        a.panel(id)?
                            .view()
                            .downcast::<crate::markets::ChartPanel>()
                            .ok()
                    })
                    .unwrap()
            };
            let was = count(cx);
            area.update(cx, |a, cx| a.remove_panel(chart, window, cx));
            assert_eq!(count(cx), was - 1);
            // Sizes are what was last drawn.
            window.render_frame(cx);
            window.render_frame(cx);
            let state = area.read(cx).dump(cx);
            (area.entity_id(), state)
        })
        .unwrap();
    fullscreen(cx, window, &markets, false);
    fullscreen(cx, window, &markets, true);
    cx.update_window(window.into(), |_, _, cx| {
        let area = markets.read(cx).workspace_area().unwrap();
        assert_eq!(area.entity_id(), before.0, "the same workspace");
        assert_eq!(area.read(cx).dump(cx), before.1, "laid out as it was");
    })
    .unwrap();
}

#[gpui_kit::test]
fn other_sections_look_the_same_in_fullscreen(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    fullscreen(cx, window, &markets, true);
    for section in [Section::Portfolio, Section::Funding, Section::History] {
        cx.update_window(window.into(), |_, window, cx| {
            desk.update(cx, |d, cx| d.show(section, window, cx));
            window.render_frame(cx);
            assert!(
                window.try_find("markets-workspace").is_none(),
                "{section:?} shows no workspace"
            );
            assert_eq!(desk.read(cx).section(), Some(section));
        })
        .unwrap();
    }
    // Back on Markets, still fullscreen: the workspace again.
    cx.update_window(window.into(), |_, window, cx| {
        desk.update(cx, |d, cx| d.show(Section::Markets, window, cx));
        window.render_frame(cx);
        assert!(window.try_find("markets-workspace").is_some());
    })
    .unwrap();
}
