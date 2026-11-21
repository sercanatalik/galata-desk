//! The watcher: its connection in settings, the Watcher section, and call
//! levels in Markets. The window's link to the watcher takes snapshots from
//! the test and records saves, so nothing here reaches a watcher.

use std::sync::Arc;

use galata_desk_core::rust_decimal::Decimal;
use galata_desk_core::secrets::{SecretError, Secrets, WATCHER_TOKEN_NAME};
use galata_desk_core::store::Store;
use galata_desk_core::watcher::{
    ConfigState, Direction, Health, WatcherConfig, WatcherError, WatcherSnapshot,
};
use gpui_kit::test::{TestEventExt, TestQueryExt, TestWindowExt};
use gpui_kit::{AppContext, Entity, TestAppContext, WindowHandle, base::Root};

use super::{ADDRESS, open_sized};
use crate::Core;
use crate::desk::Desk;
use crate::settings::Settings;
use crate::watcher_link::WatcherLink;

/// A window tall enough to show all of Settings, the Watcher panel
/// included, without scrolling.
pub fn open_tall(cx: &mut TestAppContext) -> (WindowHandle<Root>, Entity<Desk>, tempfile::TempDir) {
    open_sized(
        cx,
        true,
        gpui_kit::size(gpui_kit::px(1320.), gpui_kit::px(2000.)),
    )
}

pub const URL: &str = "http://127.0.0.1:8790";
pub const TOKEN: &str = "watcher-token-SECRET-42";

/// The configuration recorded from the real watcher: one fired and one
/// armed level, stop alerts off, watching the example address `0x…aa`.
pub fn recorded_config() -> WatcherConfig {
    let mut config: WatcherConfig = serde_json::from_str(include_str!(
        "../../../galata-desk-core/tests/fixtures/watcher/config.json"
    ))
    .unwrap();
    config.version = 1;
    config
}

/// A watcher that answered just now with `config`.
pub fn answered(config: ConfigState, now_ms: u64) -> WatcherSnapshot {
    WatcherSnapshot {
        reached_ms: Some(now_ms),
        problem: None,
        health: Some(Health {
            last_ok_ms: Some(now_ms - 40_000),
            last_error: None,
            last_error_ms: None,
            config_version: config.config().map(|c| c.version),
            address: config.config().map(|c| c.address.clone()),
        }),
        config,
        alerts: Vec::new(),
    }
}

/// Open settings over the current screen and return the page.
pub fn open_settings_page(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
) -> Entity<Settings> {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("settings", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        desk.read(cx).settings_view().unwrap()
    })
    .unwrap()
}

/// Type an address and token in the Watcher panel and press Save.
pub fn save_connection(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    settings: &Entity<Settings>,
    url: &str,
    token: &str,
) {
    cx.update_window(window.into(), |_, window, cx| {
        settings.update(cx, |view, cx| view.set_watcher_text(url, token, window, cx));
        window.render_frame(cx);
        window.click("save-watcher", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

/// Open the app with the account connected and a watcher saved, and give the
/// link `snapshot`. Returns the window, the desk and the link.
pub fn open_with_watcher(
    cx: &mut TestAppContext,
    snapshot: WatcherSnapshot,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<WatcherLink>,
    tempfile::TempDir,
) {
    let (window, desk, dir) = open_tall(cx);
    let settings = open_settings_page(cx, window, &desk);
    save_connection(cx, window, &settings, URL, TOKEN);
    let link = cx.update(|cx| desk.read(cx).watcher_link().expect("a link once saved"));
    cx.update_window(window.into(), |_, window, cx| {
        link.update(cx, |link, cx| link.set_snapshot(snapshot, cx));
        window.click("settings-done", cx);
    })
    .unwrap();
    cx.run_until_parked();
    (window, desk, link, dir)
}

fn now() -> u64 {
    crate::watcher_link::now_ms()
}

#[gpui_kit::test]
fn with_nothing_saved_there_is_no_watcher_connection(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_tall(cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(desk.read(cx).watcher_link().is_none());
        assert!(Core::get(cx).watcher_url.is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn saving_a_connection_keeps_the_token_only_in_the_keychain_and_connects(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_tall(cx);
    let settings = open_settings_page(cx, window, &desk);
    save_connection(cx, window, &settings, "  http://127.0.0.1:8790/ ", TOKEN);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let view = settings.read(cx);
        assert_eq!(
            view.watcher_token_text(cx),
            "",
            "the token field is emptied"
        );
        assert_eq!(
            view.watcher_message(),
            Some(("Saved. The token is in the keychain.".into(), false))
        );
        assert_eq!(
            Core::get(cx)
                .secrets
                .get(WATCHER_TOKEN_NAME)
                .unwrap()
                .as_deref(),
            Some(TOKEN)
        );
        assert_eq!(
            Core::get(cx)
                .watcher_url
                .as_ref()
                .map(|u| u.to_string())
                .as_deref(),
            Some(URL)
        );
        assert!(desk.read(cx).watcher_link().is_some());
        // The saved state shows the address, never the token.
        assert!(window.try_find("save-watcher").is_none());
        assert!(window.try_find("remove-watcher").is_some());
        assert!(window.find_all_by_label(TOKEN).is_empty());
    })
    .unwrap();
    // The address is remembered, and no file holds the token.
    assert_eq!(
        Store::at(dir.path())
            .settings()
            .watcher_url
            .map(|u| u.to_string())
            .as_deref(),
        Some(URL)
    );
    for entry in std::fs::read_dir(dir.path()).unwrap() {
        let text = std::fs::read_to_string(entry.unwrap().path()).unwrap_or_default();
        assert!(!text.contains(TOKEN), "a file holds the token");
    }
}

#[gpui_kit::test]
fn a_public_plain_http_address_is_refused_and_nothing_saved(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_tall(cx);
    let settings = open_settings_page(cx, window, &desk);
    save_connection(cx, window, &settings, "http://203.0.113.5:8790", TOKEN);
    cx.update_window(window.into(), |_, _, cx| {
        let (text, problem) = settings.read(cx).watcher_message().unwrap();
        assert!(problem && text.contains("unencrypted"), "{text}");
        assert!(Core::get(cx).watcher_url.is_none());
        assert_eq!(Core::get(cx).secrets.get(WATCHER_TOKEN_NAME).unwrap(), None);
        assert!(desk.read(cx).watcher_link().is_none());
    })
    .unwrap();

    save_connection(cx, window, &settings, "watcher:8790", TOKEN);
    cx.update_window(window.into(), |_, _, cx| {
        let (text, _) = settings.read(cx).watcher_message().unwrap();
        assert!(text.contains("http:// or https://"), "{text}");
    })
    .unwrap();

    save_connection(cx, window, &settings, URL, "  ");
    cx.update_window(window.into(), |_, _, cx| {
        let (text, problem) = settings.read(cx).watcher_message().unwrap();
        assert!(problem && text.contains("API token"), "{text}");
        assert!(Core::get(cx).watcher_url.is_none());
    })
    .unwrap();
}

/// A keychain that refuses everything.
struct Refusing;

impl Secrets for Refusing {
    fn get(&self, _: &str) -> Result<Option<String>, SecretError> {
        Ok(None)
    }
    fn set(&self, _: &str, _: &str) -> Result<(), SecretError> {
        Err(SecretError("locked".into()))
    }
    fn delete(&self, _: &str) -> Result<(), SecretError> {
        Err(SecretError("locked".into()))
    }
}

#[gpui_kit::test]
fn a_keychain_refusal_saves_nothing_and_says_why(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_tall(cx);
    cx.update(|cx| cx.global_mut::<Core>().secrets = Arc::new(Refusing));
    let settings = open_settings_page(cx, window, &desk);
    save_connection(cx, window, &settings, URL, TOKEN);
    cx.update_window(window.into(), |_, _, cx| {
        let (text, problem) = settings.read(cx).watcher_message().unwrap();
        assert!(problem && text.contains("keychain refused"), "{text}");
        assert!(!text.contains(TOKEN));
        assert!(Core::get(cx).watcher_url.is_none());
        assert!(desk.read(cx).watcher_link().is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn removing_the_connection_deletes_the_token_and_disconnects(cx: &mut TestAppContext) {
    let (window, desk, dir) = open_tall(cx);
    let settings = open_settings_page(cx, window, &desk);
    save_connection(cx, window, &settings, URL, TOKEN);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("remove-watcher", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(Core::get(cx).secrets.get(WATCHER_TOKEN_NAME).unwrap(), None);
        assert!(Core::get(cx).watcher_url.is_none());
        assert!(desk.read(cx).watcher_link().is_none());
        assert!(window.try_find("save-watcher").is_some());
    })
    .unwrap();
    assert_eq!(Store::at(dir.path()).settings().watcher_url, None);
}

#[gpui_kit::test]
fn disconnecting_the_account_keeps_the_watcher_connection(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) =
        open_with_watcher(cx, answered(ConfigState::Read(recorded_config()), now()));
    drop(link);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click("disconnect", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, _, cx| {
        assert!(desk.read(cx).portfolio_view().is_none());
        // No account, so no connection is held, but the saved one stays.
        assert!(desk.read(cx).watcher_link().is_none());
        assert_eq!(
            Core::get(cx)
                .secrets
                .get(WATCHER_TOKEN_NAME)
                .unwrap()
                .as_deref(),
            Some(TOKEN)
        );
        assert!(Core::get(cx).watcher_url.is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn test_connection_reports_each_outcome(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_tall(cx);
    let settings = open_settings_page(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        settings.update(cx, |view, cx| view.set_watcher_text(URL, TOKEN, window, cx));
        window.render_frame(cx);
        window.click("test-watcher", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let health = |last_ok| Health {
        last_ok_ms: last_ok,
        last_error: None,
        last_error_ms: None,
        config_version: Some(1),
        address: None,
    };
    let now = now();
    type Answer = Result<(Health, Result<WatcherConfig, WatcherError>), WatcherError>;
    let cases: Vec<(Answer, &str, bool)> = vec![
        (
            Ok((health(Some(now - 40_000)), Ok(recorded_config()))),
            "Connected. Watching 0x0000\u{2026}00aa on mainnet, configuration version 1, last checked 40 s ago.",
            false,
        ),
        (
            Err(WatcherError::TokenRefused),
            "The watcher refused the token.",
            true,
        ),
        (
            Ok((health(None), Err(WatcherError::NoConfig))),
            "Connected, but the watcher has no configuration yet. Write its first one, with its Telegram chat, as galata-watcher's README shows.",
            false,
        ),
        (
            Err(WatcherError::Unreachable),
            "Nothing answered at http://127.0.0.1:8790. Is the watcher running?",
            true,
        ),
    ];
    for (answer, text, problem) in cases {
        cx.update_window(window.into(), |_, _, cx| {
            assert!(settings.read(cx).is_testing());
            settings.update(cx, |view, cx| view.answer_test(answer, cx));
            assert_eq!(
                settings.read(cx).watcher_message(),
                Some((text.to_string(), problem))
            );
            assert!(!settings.read(cx).is_testing());
            // Testing changes nothing.
            assert!(Core::get(cx).watcher_url.is_none());
        })
        .unwrap();
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click("test-watcher", cx);
        })
        .unwrap();
        cx.run_until_parked();
    }
}

#[gpui_kit::test]
fn watch_this_account_points_the_watcher_at_the_connected_account(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) =
        open_with_watcher(cx, answered(ConfigState::Read(recorded_config()), now()));
    let settings = open_settings_page(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            settings.read(cx).notice_other_account(cx).as_deref(),
            Some("0x00000000000000000000000000000000000000aa")
        );
        window.click("watch-account", cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let saved = &link.read(cx).saved;
        assert_eq!(saved.len(), 1);
        assert_eq!(saved[0].address, ADDRESS);
        assert_eq!(
            saved[0].rest.get("telegram_chat"),
            Some(&serde_json::json!("123456789"))
        );
        assert_eq!(
            settings.read(cx).watcher_message(),
            Some(("The watcher now watches this account.".into(), false))
        );
        assert_eq!(settings.read(cx).notice_other_account(cx), None);
    })
    .unwrap();
}

#[gpui_kit::test]
fn an_unconfigured_watcher_offers_no_watch_this_account(cx: &mut TestAppContext) {
    let (window, desk, _link, _dir) = open_with_watcher(cx, answered(ConfigState::Missing, now()));
    let settings = open_settings_page(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("watch-account").is_none());
        assert!(settings.read(cx).notice_unconfigured(cx));
    })
    .unwrap();
}

// ---- The Watcher section ----

/// Open the Watcher section from the switch.
pub fn go_to_watcher(
    cx: &mut TestAppContext,
    window: WindowHandle<Root>,
    desk: &Entity<Desk>,
) -> Entity<crate::watcher::WatcherSection> {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("sections").click(4usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(desk.read(cx).section(), Some(crate::desk::Section::Watcher));
        desk.read(cx).watcher_section().unwrap()
    })
    .unwrap()
}

fn click(cx: &mut TestAppContext, window: WindowHandle<Root>, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

fn configured(
    cx: &mut TestAppContext,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<WatcherLink>,
    tempfile::TempDir,
) {
    open_with_watcher(cx, answered(ConfigState::Read(recorded_config()), now()))
}

#[gpui_kit::test]
fn the_switch_offers_watcher_after_history_only_while_connected(cx: &mut TestAppContext) {
    let (window, desk, _dir) = open_tall(cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.within("sections").try_find(4usize).is_none());
        desk.update(cx, |d, cx| {
            d.show(crate::desk::Section::Watcher, window, cx)
        });
        assert_eq!(
            desk.read(cx).section(),
            Some(crate::desk::Section::Portfolio)
        );
    })
    .unwrap();

    let (window, desk, _link, _dir) = configured(cx);
    go_to_watcher(cx, window, &desk);
    // Removing the connection leaves the section, and the switch loses it.
    let settings = open_settings_page(cx, window, &desk);
    drop(settings);
    click(cx, window, "remove-watcher");
    click(cx, window, "settings-done");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert_eq!(
            desk.read(cx).section(),
            Some(crate::desk::Section::Portfolio)
        );
        assert!(window.within("sections").try_find(4usize).is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn health_shows_normal_stalled_and_never_checked(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    let set_last_ok = |cx: &mut TestAppContext, last_ok: Option<u64>| {
        cx.update(|cx| {
            link.update(cx, |link, cx| {
                let mut s = link.snapshot().clone();
                s.health.as_mut().unwrap().last_ok_ms = last_ok;
                link.set_snapshot(s, cx);
            })
        });
    };
    cx.update(|cx| {
        assert_eq!(
            section.read(cx).health_text(cx),
            ("Checking normally".into(), false)
        )
    });
    // The recorded configuration watches the example address, not this account.
    cx.update(|cx| {
        assert_eq!(
            section.read(cx).notice_other_account(cx).as_deref(),
            Some("0x00000000000000000000000000000000000000aa")
        )
    });
    set_last_ok(cx, Some(now() - 5 * 60_000));
    cx.update(|cx| {
        assert_eq!(
            section.read(cx).health_text(cx),
            (
                "The watcher has not checked the account since 5 min ago.".into(),
                true
            )
        )
    });
    set_last_ok(cx, None);
    cx.update(|cx| {
        assert_eq!(
            section.read(cx).health_text(cx),
            ("The watcher has not checked the account yet.".into(), true)
        )
    });
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("watcher-health").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn levels_are_deleted_rearmed_and_edited_through_the_watcher(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    // Row 0 is the fired BTC level: it offers Re-arm.
    click(cx, window, ("level-rearm", 0usize));
    cx.update(|cx| {
        let saved = link.read(cx).saved.last().unwrap().clone();
        assert!(saved.levels[0].is_armed());
        assert_eq!(saved.levels[0].fired(), None);
        assert_eq!(
            section.read(cx).message(),
            Some(("Level re-armed.".into(), false))
        );
    });
    // Edit row 1 (xyz:NVDA, 210 above) to 215 below, with a note.
    click(cx, window, ("level-edit", 1usize));
    cx.update_window(window.into(), |_, window, cx| {
        section.update(cx, |s, cx| s.set_edit_text("215", "earnings", window, cx));
    })
    .unwrap();
    click(cx, window, "edit-below");
    click(cx, window, "edit-save");
    cx.update(|cx| {
        let saved = link.read(cx).saved.last().unwrap().clone();
        let nvda = &saved.levels[1];
        assert_eq!(
            (nvda.price, nvda.direction),
            (Decimal::new(215, 0), Direction::Below)
        );
        assert_eq!(nvda.note.as_deref(), Some("earnings"));
    });
    // Delete row 0.
    click(cx, window, ("level-delete", 0usize));
    cx.update(|cx| {
        let saved = link.read(cx).saved.last().unwrap().clone();
        assert_eq!(saved.levels.len(), 1);
        assert_eq!(saved.levels[0].market, "xyz:NVDA");
        assert_eq!(link.read(cx).saved.len(), 3);
    });
}

#[gpui_kit::test]
fn a_new_level_takes_its_direction_from_the_last_price(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.mids.insert("BTC".into(), Decimal::new(60_000, 0));
        })
    });
    let form = cx.update(|cx| section.read(cx).add_form());
    let add = |cx: &mut TestAppContext, market: &str, price: &str| {
        cx.update_window(window.into(), |_, window, cx| {
            form.update(cx, |f, cx| f.type_level(market, price, "", window, cx));
        })
        .unwrap();
        click(cx, window, "add-level-save");
    };
    add(cx, "BTC", "65000");
    add(cx, "BTC", "58,000");
    cx.update(|cx| {
        let saved = &link.read(cx).saved;
        assert_eq!(saved.len(), 2);
        let last = |i: usize| saved[i].levels.last().unwrap().clone();
        assert_eq!(
            (last(0).price, last(0).direction),
            (Decimal::new(65_000, 0), Direction::Above)
        );
        assert_eq!(
            (last(1).price, last(1).direction),
            (Decimal::new(58_000, 0), Direction::Below)
        );
        assert_eq!(
            section.read(cx).message(),
            Some(("Level saved. It is armed.".into(), false))
        );
    });
    // A market the venue doesn't list, and a price that isn't positive.
    add(cx, "NOPE", "5");
    cx.update(|cx| {
        assert_eq!(
            form.read(cx).message().as_deref(),
            Some("NOPE is not listed on the venue, so no level was saved.")
        );
    });
    add(cx, "BTC", "0");
    cx.update(|cx| {
        assert_eq!(
            form.read(cx).message().as_deref(),
            Some("Enter a price above zero.")
        )
    });
    cx.update(|cx| assert_eq!(link.read(cx).saved.len(), 2));
}

#[gpui_kit::test]
fn stop_alerts_are_one_switch_for_every_ticker(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    cx.update(|cx| assert!(!section.read(cx).stop_alerts_shown(cx)));
    click(cx, window, "stop-alerts");
    cx.update(|cx| {
        let saved = link.read(cx).saved.last().unwrap().clone();
        assert!(saved.stop_alerts);
        assert!(section.read(cx).stop_alerts_shown(cx));
    });
    click(cx, window, "stop-alerts");
    cx.update(|cx| assert!(!link.read(cx).saved.last().unwrap().stop_alerts));
}

#[gpui_kit::test]
fn the_pnl_summary_is_one_switch(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    cx.update(|cx| assert_eq!(section.read(cx).pnl_summary_shown(cx), Some(true)));
    click(cx, window, "pnl-summary");
    cx.update(|cx| {
        assert_eq!(link.read(cx).saved.last().unwrap().pnl_summary, Some(false));
        assert_eq!(section.read(cx).pnl_summary_shown(cx), Some(false));
        assert_eq!(
            section.read(cx).message(),
            Some(("PnL summary off.".into(), false))
        );
    });
    click(cx, window, "pnl-summary");
    cx.update(|cx| assert_eq!(link.read(cx).saved.last().unwrap().pnl_summary, Some(true)));
}

#[gpui_kit::test]
fn an_older_watcher_gets_no_pnl_summary_switch_and_no_setting(cx: &mut TestAppContext) {
    let mut older = recorded_config();
    older.pnl_summary = None;
    let (window, desk, link, _dir) =
        open_with_watcher(cx, answered(ConfigState::Read(older), now()));
    let section = go_to_watcher(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("pnl-summary").is_none());
        assert!(window.try_find("stop-alerts").is_some());
    })
    .unwrap();
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.mids.insert("BTC".into(), Decimal::new(60_000, 0));
        })
    });
    let form = cx.update(|cx| section.read(cx).add_form());
    cx.update_window(window.into(), |_, window, cx| {
        form.update(cx, |f, cx| f.type_level("BTC", "65000", "", window, cx));
    })
    .unwrap();
    click(cx, window, "add-level-save");
    cx.update(|cx| {
        let saved = link.read(cx).saved.last().unwrap().clone();
        assert_eq!(
            saved.levels.len(),
            older_levels() + 1,
            "the level was saved"
        );
        let written = serde_json::to_value(&saved).unwrap();
        assert!(written.get("pnl_summary").is_none(), "{written}");
    });
}

fn older_levels() -> usize {
    recorded_config().levels.len()
}

#[gpui_kit::test]
fn the_near_liquidation_distance_is_tuned_within_its_range(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    let set = |cx: &mut TestAppContext, text: &str| {
        cx.update_window(window.into(), |_, window, cx| {
            section.update(cx, |s, cx| s.set_near_liq_text(text, window, cx));
        })
        .unwrap();
        click(cx, window, "near-liq-save");
    };
    set(cx, "80");
    cx.update(|cx| {
        assert!(link.read(cx).saved.is_empty());
        assert_eq!(
            section.read(cx).message(),
            Some((
                "The distance is from 2% to 50%. Nothing was saved.".into(),
                true
            ))
        );
    });
    set(cx, "10");
    cx.update(|cx| {
        assert_eq!(
            link.read(cx).saved[0].near_liquidation_pct,
            Decimal::new(10, 0)
        );
    });
}

#[gpui_kit::test]
fn a_stale_watcher_offers_no_edits_until_it_answers_again(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    cx.update(|cx| {
        link.update(cx, |l, cx| {
            let mut s = l.snapshot().clone();
            s.problem = Some(galata_desk_core::watcher::Problem::Unreachable);
            s.reached_ms = Some(now() - 120_000);
            l.set_snapshot(s, cx);
        })
    });
    cx.update(|cx| {
        let banner = section.read(cx).banner(cx).unwrap();
        assert!(
            banner.starts_with(
                "The watcher could not be reached at this address. Showing what was read 2 min ago."
            ),
            "{banner}"
        );
    });
    // The levels are still listed, but Delete does nothing.
    click(cx, window, ("level-delete", 0usize));
    cx.update(|cx| assert!(link.read(cx).saved.is_empty()));

    cx.update(|cx| {
        link.update(cx, |l, cx| {
            let mut s = l.snapshot().clone();
            s.problem = None;
            s.reached_ms = Some(now());
            l.set_snapshot(s, cx);
        })
    });
    click(cx, window, ("level-delete", 0usize));
    cx.update(|cx| {
        assert_eq!(section.read(cx).banner(cx), None);
        assert_eq!(link.read(cx).saved.len(), 1);
    });
}

#[gpui_kit::test]
fn a_change_elsewhere_or_a_refusal_is_reported_and_nothing_overwritten(cx: &mut TestAppContext) {
    let (window, desk, link, _dir) = configured(cx);
    let section = go_to_watcher(cx, window, &desk);
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.next_outcome = Some(WatcherError::Changed { version: 2 })
        })
    });
    click(cx, window, ("level-delete", 0usize));
    cx.update(|cx| {
        assert!(link.read(cx).saved.is_empty());
        assert_eq!(
            section.read(cx).message(),
            Some((
                "Not saved: the configuration changed elsewhere. The newer one is shown; make the change again if you still want it.".into(),
                true
            ))
        );
    });
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.next_outcome = Some(WatcherError::Refused {
                problems: vec![
                    "`levels[0].price`: must be above zero".into(),
                    "`address`: bad".into(),
                ],
            })
        })
    });
    click(cx, window, ("level-delete", 0usize));
    cx.update(|cx| {
        assert_eq!(
            section.read(cx).message(),
            Some((
                "Not saved. The watcher found: `levels[0].price`: must be above zero; `address`: bad".into(),
                true
            ))
        );
    });
}

#[gpui_kit::test]
fn the_alert_history_is_hidden_until_asked_for_and_lists_late_and_undelivered_alerts(
    cx: &mut TestAppContext,
) {
    let mut snapshot = answered(ConfigState::Read(recorded_config()), now());
    let recorded: serde_json::Value = serde_json::from_str(include_str!(
        "../../../galata-desk-core/tests/fixtures/watcher/alerts.json"
    ))
    .unwrap();
    snapshot.alerts = serde_json::from_value(recorded["alerts"].clone()).unwrap();
    let (window, desk, _link, _dir) = open_with_watcher(cx, snapshot);
    let section = go_to_watcher(cx, window, &desk);
    let shown = |cx: &mut TestAppContext| {
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find(("alert", 0usize)).is_some()
        })
        .unwrap()
    };
    assert!(!shown(cx), "hidden when the section opens");
    assert!(cx.update(|cx| !section.read(cx).history_shown()));

    click(cx, window, "alert-history");
    assert!(shown(cx));
    cx.update_window(window.into(), |_, window, _| {
        // Newest first: the late, undelivered stop-loss, then the level.
        assert!(window.try_find(("alert", 1usize)).is_some());
        assert!(window.try_find(("alert-late", 0usize)).is_some());
        assert!(window.try_find(("alert-undelivered", 0usize)).is_some());
        assert!(window.try_find(("alert-late", 1usize)).is_none());
        assert!(window.try_find(("alert-delivered", 1usize)).is_some());
        // Listed below the health line.
        let line = window.find("watcher-health").bounds();
        let list = window.find("alert-history-list").bounds();
        assert!(list.origin.y > line.origin.y);
    })
    .unwrap();

    click(cx, window, "alert-history");
    assert!(!shown(cx), "a second press hides it");
}

#[gpui_kit::test]
fn call_levels_come_first_and_the_health_line_last(cx: &mut TestAppContext) {
    let (window, desk, _link, _dir) = configured(cx);
    let _section = go_to_watcher(cx, window, &desk);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let level = window.find(("level-delete", 0usize)).bounds();
        let stops = window.find("stop-alerts").bounds();
        let summary = window.find("pnl-summary").bounds();
        let near_liq = window.find("near-liq-save").bounds();
        let health = window.find("watcher-health").bounds();
        assert!(level.origin.y < stops.origin.y, "levels above stop alerts");
        assert!(
            stops.origin.y < summary.origin.y,
            "the PnL summary below stop alerts"
        );
        // Near liquidation is the panel beside the stop alerts panel.
        assert!(
            summary.origin.x < near_liq.origin.x,
            "and before near liquidation"
        );
        assert!(stops.origin.y < health.origin.y, "health line last");
    })
    .unwrap();
}

// ---- Call levels in Markets ----

/// A configuration whose levels in BTC are: 130 armed with a note, 105
/// fired, 200 armed (above the candles, which run from 99 to 141); and an
/// armed ETH level at 120.
fn chart_config() -> WatcherConfig {
    let mut config = recorded_config();
    let level = |market: &str, price: i64, note: Option<&str>| {
        galata_desk_core::watcher::Level::new(
            market,
            Decimal::from(price),
            Direction::Above,
            note.map(Into::into),
        )
    };
    let mut fired = level("BTC", 105, None);
    fired.state = Some(galata_desk_core::watcher::LevelState::Fired);
    fired.fired_at = Some(now() - 60_000);
    config.levels = vec![
        level("BTC", 130, Some("resistance")),
        fired,
        level("BTC", 200, None),
        level("ETH", 120, None),
    ];
    config
}

fn markets_with_chart(
    cx: &mut TestAppContext,
    config: ConfigState,
) -> (
    WindowHandle<Root>,
    Entity<Desk>,
    Entity<WatcherLink>,
    Entity<crate::markets::Markets>,
    tempfile::TempDir,
) {
    let (window, desk, link, dir) = open_with_watcher(cx, answered(config, now()));
    Store::at(dir.path())
        .save_watchlist(&super::watchlist(&["BTC", "ETH"]))
        .unwrap();
    let markets = super::go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(super::charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        link.update(cx, |l, _| {
            l.mids.insert("BTC".into(), Decimal::from(120));
        });
        window.render_frame(cx);
    })
    .unwrap();
    (window, desk, link, markets, dir)
}

#[gpui_kit::test]
fn armed_levels_are_drawn_on_the_selected_markets_chart(cx: &mut TestAppContext) {
    let (window, _desk, link, _markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // 130 is inside the candles' range: a line. 105 has fired: none.
        // 200 is above: a tag at the top edge. ETH's level is not BTC's.
        assert!(window.try_find(("call-line", 0usize)).is_some());
        assert!(window.try_find(("call-line", 1usize)).is_none());
        assert!(window.try_find("call-above").is_some());
        assert!(window.try_find("call-below").is_none());

        // A watcher watching another network draws nothing here.
        link.update(cx, |l, cx| {
            let mut s = l.snapshot().clone();
            if let ConfigState::Read(c) = &mut s.config {
                c.network = galata_desk_core::network::Network::Testnet;
            }
            l.set_snapshot(s, cx);
        });
        window.render_frame(cx);
        assert!(window.try_find(("call-line", 0usize)).is_none());
        assert!(window.try_find("call-above").is_none());
    })
    .unwrap();
}

/// Right-click the chart `y` pixels from the plot's top.
fn right_click_chart(cx: &mut TestAppContext, window: WindowHandle<Root>, y: f32) {
    use gpui_kit::{MouseButton, point, px};
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let plot = window.find("chart-plot").bounds();
        let at = plot.origin + point(px(20.), px(y));
        window.pointer_move(at, None, cx);
        window.click_with_options(
            "chart-plot",
            gpui_kit::test::ClickOptions::new()
                .with_offset(point(px(20.), px(y)))
                .with_button(MouseButton::Right),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn alert_at_this_price_adds_an_armed_level_from_the_chart(cx: &mut TestAppContext) {
    let (window, _desk, link, _markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    // 77 px from the top is 130.008 on a 99 to 141 plot: 130 at its precision.
    right_click_chart(cx, window, 77.);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(0usize, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update(|cx| {
        let saved = &link.read(cx).saved;
        assert_eq!(saved.len(), 1);
        let added = saved[0].levels.last().unwrap();
        assert_eq!(added.market, "BTC");
        assert_eq!(added.price, Decimal::from(130));
        assert!(added.is_armed());
        // The chat and digest went back unchanged.
        assert_eq!(
            saved[0].rest.get("telegram_chat"),
            Some(&serde_json::json!("123456789"))
        );
    });
}

#[gpui_kit::test]
fn the_chart_offers_no_alert_when_the_watcher_cannot_take_one(cx: &mut TestAppContext) {
    // Unreachable.
    let (window, _desk, link, _markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    cx.update(|cx| {
        link.update(cx, |l, cx| {
            let mut s = l.snapshot().clone();
            s.problem = Some(galata_desk_core::watcher::Problem::Unreachable);
            l.set_snapshot(s, cx);
        })
    });
    right_click_chart(cx, window, 77.);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();

    // Not configured.
    let (window, _desk, _link, _markets, _dir) = markets_with_chart(cx, ConfigState::Missing);
    right_click_chart(cx, window, 77.);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();

    // Watching another network.
    let mut other = chart_config();
    other.network = galata_desk_core::network::Network::Testnet;
    let (window, _desk, _link, _markets, _dir) = markets_with_chart(cx, ConfigState::Read(other));
    right_click_chart(cx, window, 77.);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();
}

// ---- Alerts from a Markets tile ----

fn right_click_tile(cx: &mut TestAppContext, window: WindowHandle<Root>, ix: usize) {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.right_click(("tile", ix), cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_tile_opens_an_alert_form_that_saves_an_armed_level(cx: &mut TestAppContext) {
    let (window, _desk, link, markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.mids.insert("ETH".into(), Decimal::from(3300));
        })
    });
    // Tile 1 is ETH.
    right_click_tile(cx, window, 1);
    click_menu_item(cx, window, 0);
    let form = cx
        .update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            assert!(window.try_find("tile-alert-form").is_some());
            markets.read(cx).tile_form(cx).expect("the form is open")
        })
        .unwrap();
    cx.update_window(window.into(), |_, window, cx| {
        form.update(cx, |f, cx| {
            f.type_level("", "3,450", "breakout", window, cx)
        });
    })
    .unwrap();
    click(cx, window, "tile-alert-save");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let saved = &link.read(cx).saved;
        assert_eq!(saved.len(), 1);
        let added = saved[0].levels.last().unwrap();
        assert_eq!(
            (added.market.as_str(), added.price),
            ("ETH", Decimal::from(3450))
        );
        assert_eq!(added.direction, Direction::Above);
        assert_eq!(added.note.as_deref(), Some("breakout"));
        // Saved: the form closes.
        assert!(window.try_find("tile-alert-form").is_none());
    })
    .unwrap();

    // Opened again and cancelled: nothing is saved.
    right_click_tile(cx, window, 1);
    click_menu_item(cx, window, 0);
    click(cx, window, "tile-alert-cancel");
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("tile-alert-form").is_none());
        assert_eq!(link.read(cx).saved.len(), 1);
    })
    .unwrap();

    // A tile's own click, hover and keys are unchanged.
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(("tile", 1usize), cx);
        window.render_frame(cx);
        assert_eq!(markets.read(cx).selected_market(cx).as_deref(), Some("ETH"));
        window.hover(("tile", 0usize), cx);
        window.render_frame(cx);
        assert!(window.try_find(("remove", 0usize)).is_some());
    })
    .unwrap();
}

fn click_menu_item(cx: &mut TestAppContext, window: WindowHandle<Root>, ix: usize) {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.within("popup-menu").click(ix, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_tile_offers_no_alert_when_the_watcher_cannot_take_one(cx: &mut TestAppContext) {
    // No watcher at all.
    let (window, desk, dir) = open_tall(cx);
    Store::at(dir.path())
        .save_watchlist(&super::watchlist(&["BTC", "ETH"]))
        .unwrap();
    let markets = super::go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(super::charted("BTC", &["BTC", "ETH"], 40), cx)
        });
        window.render_frame(cx);
    })
    .unwrap();
    right_click_tile(cx, window, 1);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();

    // Unreachable.
    let (window, _desk, link, _markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    cx.update(|cx| {
        link.update(cx, |l, cx| {
            let mut s = l.snapshot().clone();
            s.problem = Some(galata_desk_core::watcher::Problem::Unreachable);
            l.set_snapshot(s, cx);
        })
    });
    right_click_tile(cx, window, 1);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();

    // Watching another network.
    let mut other = chart_config();
    other.network = galata_desk_core::network::Network::Testnet;
    let (window, _desk, _link, _markets, _dir) = markets_with_chart(cx, ConfigState::Read(other));
    right_click_tile(cx, window, 1);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        assert!(window.try_find("popup-menu").is_none());
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_tiles_alert_form_closes_by_cancel_close_escape_or_a_click_outside(cx: &mut TestAppContext) {
    let coins = [
        "BTC", "ETH", "SOL", "HYPE", "DOGE", "XRP", "AVAX", "LINK", "SUI", "TON", "ADA", "DOT",
    ];
    let (window, desk, link, dir) =
        open_with_watcher(cx, answered(ConfigState::Read(chart_config()), now()));
    Store::at(dir.path())
        .save_watchlist(&super::watchlist(&coins))
        .unwrap();
    let markets = super::go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(super::charted("BTC", &coins, 40), cx)
        });
        window.render_frame(cx);
    })
    .unwrap();
    let form_shown = |cx: &mut TestAppContext, window: WindowHandle<Root>| {
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            window.try_find("tile-alert-form").is_some()
        })
        .unwrap()
    };

    let open = |cx: &mut TestAppContext| {
        // The last tile, on the bottom row.
        right_click_tile(cx, window, 11);
        click_menu_item(cx, window, 0);
        assert!(form_shown(cx, window), "opened");
    };
    open(cx);
    click(cx, window, "tile-alert-cancel");
    assert!(!form_shown(cx, window), "Cancel closes it");

    open(cx);
    click(cx, window, "tile-alert-close");
    assert!(!form_shown(cx, window), "the close button closes it");

    open(cx);
    cx.update_window(window.into(), |_, window, cx| window.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!form_shown(cx, window), "Esc closes it");

    open(cx);
    click(cx, window, ("tile", 0usize));
    assert!(!form_shown(cx, window), "a click elsewhere closes it");

    cx.update(|cx| assert!(link.read(cx).saved.is_empty(), "nothing saved"));
}

#[gpui_kit::test]
fn a_tiles_alert_form_opens_above_the_tile_when_there_is_no_room_below(cx: &mut TestAppContext) {
    let coins = [
        "BTC", "ETH", "SOL", "HYPE", "DOGE", "XRP", "AVAX", "LINK", "SUI", "TON", "ADA", "DOT",
    ];
    let (window, desk, _link, dir) =
        open_with_watcher(cx, answered(ConfigState::Read(chart_config()), now()));
    Store::at(dir.path())
        .save_watchlist(&super::watchlist(&coins))
        .unwrap();
    let markets = super::go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| {
            view.set_snapshot(super::charted("BTC", &coins, 40), cx)
        });
        window.render_frame(cx);
    })
    .unwrap();
    // Make the window end just under the last tile.
    let last_bottom = cx
        .update_window(window.into(), |_, window, _| {
            window.find(("tile", 11usize)).bounds().bottom()
        })
        .unwrap();
    cx.simulate_window_resize(
        window.into(),
        gpui_kit::size(gpui_kit::px(1320.), last_bottom + gpui_kit::px(20.)),
    );
    cx.run_until_parked();
    let measure = |cx: &mut TestAppContext, ix: usize| {
        right_click_tile(cx, window, ix);
        click_menu_item(cx, window, 0);
        cx.update_window(window.into(), |_, window, cx| {
            window.render_frame(cx);
            let tile = window.find(("tile", ix)).bounds();
            let form = window.find("tile-alert-form").bounds();
            let view = window.viewport_size();
            (tile, form, view)
        })
        .unwrap()
    };

    let (tile, form, view) = measure(cx, 11);
    assert!(
        form.bottom() <= tile.top(),
        "above the bottom-row tile: {form:?} {tile:?}"
    );
    assert!(form.bottom() <= view.height && form.top() >= gpui_kit::px(0.));
    assert!(form.right() <= view.width, "inside the window's right edge");
    click(cx, window, "tile-alert-cancel");

    let (tile, form, _) = measure(cx, 0);
    assert!(
        form.top() >= tile.bottom(),
        "below a tile with room under it"
    );
}

// ---- The Markets position panel's alerts bell ----

fn click_and_settle(cx: &mut TestAppContext, window: WindowHandle<Root>, id: &'static str) {
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        window.click(id, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(window.into(), |_, window, cx| window.render_frame(cx))
        .unwrap();
}

#[gpui_kit::test]
fn the_bell_counts_the_markets_armed_levels(cx: &mut TestAppContext) {
    let (window, _desk, _link, _markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        // BTC has two armed levels and one fired.
        assert!(window.try_find_by_label("2 armed").is_some());
        let bell = window.find_by_label("Alerts for BTC");
        assert_ne!(bell.disabled(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn without_a_usable_watcher_the_bell_says_why_and_cannot_be_pressed(cx: &mut TestAppContext) {
    // No watcher at all.
    let (window, desk, _dir) = super::open_with(cx, Some(&["BTC"]));
    super::go_to_markets(window, &desk, cx);
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let bell = window.find_by_label("Alerts for BTC: needs a watcher connection");
        assert_eq!(bell.disabled(), Some(true));
    })
    .unwrap();
    click_and_settle(cx, window, "position-alerts");
    cx.update_window(window.into(), |_, window, _| {
        assert!(window.try_find("alerts-menu").is_none());
    })
    .unwrap();

    // Watching another network.
    let mut other = chart_config();
    other.network = galata_desk_core::network::Network::Testnet;
    let (window, _desk, _link, _markets, _dir) = markets_with_chart(cx, ConfigState::Read(other));
    cx.update_window(window.into(), |_, window, cx| {
        window.render_frame(cx);
        let bell = window.find_by_label("Alerts for BTC: the watcher watches another network");
        assert_eq!(bell.disabled(), Some(true));
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_level_added_from_the_bell_is_saved_armed_and_drawn(cx: &mut TestAppContext) {
    let (window, _desk, link, markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    click_and_settle(cx, window, "position-alerts");
    cx.update_window(window.into(), |_, window, cx| {
        assert!(window.try_find("alerts-menu").is_some());
        // The market's levels, armed first.
        assert!(window.try_find_by_label("Above 130 \u{b7} armed").is_some());
        assert!(window.try_find_by_label("Above 105 \u{b7} fired").is_some());
        markets.update(cx, |view, cx| view.set_alert_price("150", window, cx));
    })
    .unwrap();
    click_and_settle(cx, window, "alert-add");
    cx.update_window(window.into(), |_, window, cx| {
        let saved = link.read(cx).saved.last().cloned().expect("a save");
        let added = saved
            .levels
            .iter()
            .find(|l| l.market == "BTC" && l.price == Decimal::from(150))
            .expect("the new level");
        // BTC is at 120: 150 is above it.
        assert_eq!(added.direction, Direction::Above);
        assert!(added.is_armed());
        assert!(window.try_find_by_label("Alert saved.").is_some());
        // The menu lists it with the market's other levels. (The test link
        // arms every level it saves without an id, so the count is not
        // checked here.)
        assert!(window.try_find_by_label("Above 150 \u{b7} armed").is_some());
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_every_market_settings_save_through_the_watcher_and_a_conflict_overwrites_nothing(
    cx: &mut TestAppContext,
) {
    let (window, _desk, link, markets, _dir) =
        markets_with_chart(cx, ConfigState::Read(chart_config()));
    click_and_settle(cx, window, "position-alerts");
    cx.update_window(window.into(), |_, window, _| {
        assert!(
            window
                .try_find_by_label("Take-profit and stop-loss alerts, every market")
                .is_some()
        );
    })
    .unwrap();

    // The recorded configuration has stop alerts off: switching turns them on.
    click_and_settle(cx, window, "alert-stop-alerts");
    cx.update(|cx| {
        let saved = link.read(cx).saved.clone();
        assert_eq!(saved.len(), 1);
        assert!(saved[0].stop_alerts);
    });

    // The distance, within its range.
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| view.set_alert_near_liq("8", window, cx));
    })
    .unwrap();
    click_and_settle(cx, window, "alert-near-liq-save");
    cx.update(|cx| {
        let saved = link.read(cx).saved.clone();
        assert_eq!(saved.len(), 2);
        assert_eq!(saved[1].near_liquidation_pct, Decimal::from(8));
    });

    // Out of range: nothing is sent.
    cx.update_window(window.into(), |_, window, cx| {
        markets.update(cx, |view, cx| view.set_alert_near_liq("80", window, cx));
    })
    .unwrap();
    click_and_settle(cx, window, "alert-near-liq-save");
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(link.read(cx).saved.len(), 2);
        assert!(
            window
                .try_find_by_label("The distance is from 2% to 50%. Nothing was saved.")
                .is_some()
        );
    })
    .unwrap();

    // Changed elsewhere since it was read: not overwritten, and said so.
    cx.update(|cx| {
        link.update(cx, |l, _| {
            l.next_outcome = Some(WatcherError::Changed { version: 9 })
        })
    });
    click_and_settle(cx, window, "alert-stop-alerts");
    cx.update_window(window.into(), |_, window, cx| {
        assert_eq!(link.read(cx).saved.len(), 2);
        let message = window.find("alert-message");
        assert!(
            message
                .label()
                .is_some_and(|l| l.starts_with("Not saved: the configuration changed elsewhere")),
            "{:?}",
            message.label()
        );
    })
    .unwrap();
}
