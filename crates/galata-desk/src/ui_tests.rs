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
use crate::markets::Markets;

const ADDRESS: &str = "0x010461c14e146ac35fe42271bdc1134ee31c703a";

fn open(
    cx: &mut TestAppContext,
    remembered: bool,
) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    open_sized(cx, remembered, size(px(1320.), px(860.)))
}

fn open_sized(
    cx: &mut TestAppContext,
    remembered: bool,
    window_size: gpui_kit::Size<gpui_kit::Pixels>,
) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::at(dir.path());
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
        let stored = store.settings();
        let store_network = stored.network;
        cx.set_global(Core {
            runtime: Arc::new(runtime),
            store,
            // Tests never touch the real keychain.
            secrets: Arc::new(galata_desk_core::secrets::MemorySecrets::default()),
            network: store_network,
            allow_mainnet_trading: stored.allow_mainnet_trading,
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
    (window, desk, dir)
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
        assert_eq!(markets.read(cx).selections(), "period=24H colour=Change");
        choose_period(window, "7 days", cx);
        window.within("colour").click(2usize, cx);
        assert_eq!(markets.read(cx).selections(), "period=7D colour=Funding");
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
        assert_eq!(markets.read(cx).selections(), "period=7D colour=Funding");
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
                    .selections()
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
                    .selections()
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
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("BTC"));
        assert!(window.try_find("market-panel").is_some());

        // Pointing at tiles changes nothing but which one offers Remove.
        window.hover(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).hovered_market().as_deref(), Some("ETH"));
        window.hover(("tile", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("BTC"));

        // A click selects.
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("ETH"));

        // Clicking the selected tile leaves it selected.
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("ETH"));
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
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("BTC"));
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("ETH"));

        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("space", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("SOL"));
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
        assert_eq!(markets.read(cx).watched(), ["BTC", "SOL"]);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("SOL"));

        // Remove it again, now the last in the list: the one before it.
        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("delete", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("BTC"));
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
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("SOL"));
    })
    .unwrap();
}

#[gpui_kit::test]
fn with_nothing_watched_there_is_no_panel(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&[]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(markets.read(cx).selected_market(), None);
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
        assert_eq!(markets.read(cx).watched(), ["BTC", "SOL"]);

        // From the keyboard: focus a tile and press Backspace.
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        window.press("backspace", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(), ["SOL"]);
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
        assert!(markets.read(cx).watched().is_empty());
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
        assert_eq!(markets.read(cx).watched(), ["BTC", "xyz:TSLA"]);

        // It now reads as watched; choosing it again adds nothing.
        window.click(("pick", 3usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(), ["BTC", "xyz:TSLA"]);
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
        assert_eq!(markets.read(cx).watched().len(), LIMIT);
        assert!(!markets.read(cx).watched().contains(&"BTC".to_string()));
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_first_watchlist_is_what_the_account_holds(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_with(cx, None);
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, _, cx| {
        // Positions have not loaded: nothing is decided yet.
        assert!(markets.read(cx).watched().is_empty());
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
        assert_eq!(markets.read(cx).watched(), ["ETH", "xyz:NVDA", "BTC"]);
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
        assert_eq!(markets.read(cx).watched(), ["BTC", "ETH", "SOL", "HYPE"]);
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
        assert_eq!(markets.read(cx).watched(), ["SOL", "xyz:NVDA", "BTC"]);
    })
    .unwrap();
}

#[gpui_kit::test]
fn no_control_in_the_markets_section_trades(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_with(cx, Some(&["BTC", "ETH"]));
    let markets = go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| view.focus_market("BTC", window, cx));
        window.render_frame(cx);
        for forbidden in [
            "Trade", "Buy", "Sell", "Long", "Short", "Close", "Limit", "Market", "Deposit",
            "Withdraw", "Send",
        ] {
            assert!(
                window.find_all_by_label(forbidden).is_empty(),
                "a `{forbidden}` control is in a read-only section"
            );
        }
        // Activating a tile does nothing but focus it.
        window.click(("tile", 0usize), cx);
        window.press("enter", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).watched(), ["BTC", "ETH"]);
        assert_eq!(desk.read(cx).section(), Some(Section::Markets));
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
        assert_eq!(markets.read(cx).drawn(), ["BTC", "SOL", "ETH"]);

        choose_sort(window, "Change, gainers first", cx);
        assert_eq!(markets.read(cx).drawn(), ["SOL", "ETH", "BTC"]);
        // Only the drawing order changed.
        assert_eq!(markets.read(cx).watched(), ["BTC", "SOL", "ETH"]);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("BTC"));
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // A tile keeps its identity wherever it is drawn: this is ETH's.
        window.click(("tile", 2usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("ETH"));
        choose_sort(window, "Change, losers first", cx);
        assert_eq!(markets.read(cx).drawn(), ["BTC", "ETH", "SOL"]);
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
        assert_eq!(markets.read(cx).drawn(), ["SOL", "ETH", "BTC"]);

        // BTC overtakes: within the interval the tiles stay where they are.
        markets.update(cx, |view, cx| {
            view.set_snapshot(moving(&[("BTC", 2.0), ("SOL", 1.3), ("ETH", 0.2)]), cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(), ["SOL", "ETH", "BTC"]);

        // Once the interval has passed, the order catches up.
        markets.update(cx, |view, cx| {
            view.age_order();
            cx.notify();
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(), ["BTC", "SOL", "ETH"]);

        // A market removed is gone from the order at once.
        markets.update(cx, |view, cx| view.focus_market("SOL", window, cx));
        window.render_frame(cx);
        window.press("delete", cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn(), ["BTC", "ETH"]);
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
        assert_eq!(markets.read(cx).drawn(), ["SOL", "BTC"]);
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
        assert_eq!(markets.read(cx).drawn(), ["BTC", "SOL"]);
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
        assert!(markets.read(cx).selections().ends_with("colour=Funding"));
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
            allow_mainnet_trading: false,
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
        assert_eq!(markets.read(cx).drawn_chart().0, "loading");
        assert!(window.try_find("chart-loading").is_some());
        assert!(window.try_find("chart-failed").is_none());

        // With reduced motion asked for, the placeholders hold still and the
        // text still says what is loading.
        cx.set_reduce_motion(true);
        markets.update(cx, |_, cx| cx.notify());
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart().0, "loading");
        assert!(window.try_find("chart-loading-still").is_some());
        assert!(window.try_find("chart-loading").is_none());
        cx.set_reduce_motion(false);

        // The load failed: the chart area says so.
        let mut failed = snapshot(&["BTC", "ETH"], &["BTC", "ETH"]);
        failed.chart_failed = true;
        markets.update(cx, |view, cx| view.set_snapshot(failed, cx));
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart().0, "failed");
        assert!(window.try_find("chart-failed").is_some());
        assert!(window.try_find("chart-loading").is_none());

        // A later attempt succeeds: the chart, and no message.
        markets.update(cx, |view, cx| {
            view.set_snapshot(charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        window.render_frame(cx);
        assert_eq!(markets.read(cx).drawn_chart().0, "chart");
        assert!(window.try_find("chart-failed").is_none());
        assert!(window.try_find("chart-loading").is_none());
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
        assert_eq!(markets.read(cx).drawn_chart(), ("chart", 0, 0, None));

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
        assert_eq!(markets.read(cx).drawn_chart(), ("chart", 1, 2, None));
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
        assert_eq!(markets.read(cx).selected_market().as_deref(), Some("ETH"));
        assert_eq!(markets.read(cx).drawn_chart(), ("chart", 1, 0, None));
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
        let (state, bought, sold, since) = markets.read(cx).drawn_chart();
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
fn the_legend_explains_the_bar_and_says_when_pnl_is_partial(cx: &mut TestAppContext) {
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
        assert!(
            text_at(window, "legend-bar")
                .unwrap()
                .starts_with("Bar: period low to high")
        );
        assert!(
            text_at(window, "legend-strip")
                .unwrap()
                .starts_with("Position and PNL: yours")
        );
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
                "HYPE, last 100, -2.70%, -2.5 standard deviations from the period average, \
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
        assert_eq!(markets.read(cx).drawn(), ["ETH", "SOL", "BTC"]);
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
