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
        cx.set_global(Core {
            runtime: Arc::new(runtime),
            store,
            // Nothing listens here: every venue request fails at once.
            info_url: "http://127.0.0.1:9/info".into(),
            ws_url: "ws://127.0.0.1:9/ws".into(),
        });
        let (window, desk) = gpui_kit::open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(Bounds {
                    origin: Point::default(),
                    size: size(px(1320.), px(860.)),
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
