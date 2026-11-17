//! The window: the connect screen until an account is connected, then its
//! portfolio.

use galata_desk_core::connect::{Attempt, AttemptEvent};
use galata_desk_core::store::Account;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme, Sizable, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Subscription, Window, div};

use crate::Core;
use crate::connect::{self, ConnectState};
use crate::markets::Markets;
use crate::portfolio::{Portfolio, PortfolioEvent};
use crate::theme::Palette;

/// What is shown while an account is connected.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Portfolio,
    Markets,
    Funding,
}

enum Screen {
    Connect(ConnectState),
    /// Both sections live for as long as the account is connected, so each
    /// keeps its selections and the portfolio keeps refreshing under Markets.
    Connected {
        section: Section,
        portfolio: Entity<Portfolio>,
        /// Created, and its stream opened, the first time Markets is chosen.
        markets: Option<Entity<Markets>>,
        _events: [Subscription; 2],
    },
}

pub struct Desk {
    screen: Screen,
}

impl Desk {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A remembered account opens straight to its portfolio.
        let screen = match Core::get(cx).store.account() {
            Some(account) => Self::portfolio(account, window, cx),
            None => Screen::Connect(ConnectState::Idle { notice: None }),
        };
        Self { screen }
    }

    fn portfolio(account: Account, window: &mut Window, cx: &mut Context<Self>) -> Screen {
        let view = cx.new(|cx| Portfolio::new(account.address, window, cx));
        let events = cx.subscribe(&view, |this, _, event, cx| match event {
            PortfolioEvent::Disconnect => this.disconnect(cx),
        });
        // The window draws the portfolio's freshness and footer itself, in
        // every section, so it redraws when the portfolio changes.
        let changes = cx.observe(&view, |_, _, cx| cx.notify());
        #[allow(unused_mut)]
        let mut screen = Screen::Connected {
            section: Section::Portfolio,
            portfolio: view,
            markets: None,
            _events: [events, changes],
        };
        #[cfg(feature = "shot")]
        if std::env::var("GALATA_DESK_SHOT_STATE")
            .unwrap_or_default()
            .split(',')
            .any(|kv| kv == "section=markets")
        {
            Self::open_section(&mut screen, Section::Markets, window, cx);
        }
        #[cfg(feature = "shot")]
        if std::env::var("GALATA_DESK_SHOT_STATE")
            .unwrap_or_default()
            .split(',')
            .any(|kv| kv == "section=funding")
        {
            Self::open_section(&mut screen, Section::Funding, window, cx);
        }
        screen
    }

    /// Show a section, creating the Markets view on its first use.
    fn open_section(screen: &mut Screen, to: Section, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Connected {
            section,
            portfolio,
            markets,
            ..
        } = screen
        {
            if to == Section::Markets && markets.is_none() {
                let portfolio = portfolio.clone();
                *markets = Some(cx.new(|cx| Markets::new(portfolio, window, cx)));
            }
            *section = to;
        }
    }

    pub fn show(&mut self, to: Section, window: &mut Window, cx: &mut Context<Self>) {
        Self::open_section(&mut self.screen, to, window, cx);
        cx.notify();
    }

    fn idle(&mut self, notice: Option<&str>, cx: &mut Context<Self>) {
        self.screen = Screen::Connect(ConnectState::Idle {
            notice: notice.map(|n| n.to_string().into()),
        });
        cx.notify();
    }

    /// Forget the account and go back to the connect screen. Dropping the
    /// portfolio view stops its refreshing.
    fn disconnect(&mut self, cx: &mut Context<Self>) {
        let notice = Core::get(cx)
            .store
            .forget()
            .err()
            .map(|e| format!("The stored account could not be removed: {e}"));
        self.idle(notice.as_deref(), cx);
    }

    pub fn cancel_connect(&mut self, cx: &mut Context<Self>) {
        // Replacing the state drops the task that holds the attempt.
        self.idle(None, cx);
    }

    pub fn start_connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = Core::get(cx);
        let runtime = core.runtime.clone();
        let project_id = core.store.project_id();

        let task = cx.spawn_in(window, async move |this, cx| {
            // The bridge needs the I/O runtime to listen; this executor only
            // waits for what it reports.
            let started = runtime.spawn(Attempt::start(project_id)).await;
            let mut attempt = match started {
                Ok(Ok(attempt)) => attempt,
                Ok(Err(error)) => {
                    let _ = this.update(cx, |this, cx| this.idle(Some(&error.to_string()), cx));
                    return;
                }
                Err(_) => {
                    let _ = this.update(cx, |this, cx| {
                        this.idle(Some("The local connection server could not start."), cx)
                    });
                    return;
                }
            };

            let url = attempt.url().to_string();
            let waiting = this.update(cx, |this, cx| {
                if let Screen::Connect(ConnectState::Starting { _task }) = std::mem::replace(
                    &mut this.screen,
                    Screen::Connect(ConnectState::Idle { notice: None }),
                ) {
                    this.screen = Screen::Connect(ConnectState::Waiting {
                        url: url.clone().into(),
                        rejected: None,
                        _task,
                    });
                    cx.open_url(&url);
                    cx.notify();
                    true
                } else {
                    false
                }
            });
            if !matches!(waiting, Ok(true)) {
                return;
            }

            while let Some(event) = attempt.next_event().await {
                let done = this.update_in(cx, |this, window, cx| {
                    this.on_attempt_event(event, window, cx)
                });
                if !matches!(done, Ok(false)) {
                    return;
                }
            }
        });
        self.screen = Screen::Connect(ConnectState::Starting { _task: task });
        cx.notify();
    }

    /// Returns whether the attempt is over.
    fn on_attempt_event(
        &mut self,
        event: AttemptEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        match event {
            AttemptEvent::Rejected(_) => {
                if let Screen::Connect(ConnectState::Waiting { rejected, .. }) = &mut self.screen {
                    *rejected = Some(
                        "The proof did not match: the signature was not made by the address the wallet reported. The account was not connected. You can try again in the browser page."
                            .into(),
                    );
                    cx.notify();
                }
                false
            }
            AttemptEvent::Expired => {
                self.idle(
                    Some("The connection attempt expired after five minutes. Start again."),
                    cx,
                );
                true
            }
            AttemptEvent::Connected(address) => {
                let account = Account::proven_now(address);
                let stored = Core::get(cx).store.remember(&account);
                self.screen = Self::portfolio(account, window, cx);
                if let (Err(error), Screen::Connected { portfolio, .. }) = (stored, &self.screen) {
                    portfolio.update(cx, |view, cx| {
                        view.warn(
                            format!("The account could not be remembered for next launch: {error}"),
                            cx,
                        )
                    });
                }
                // Bring the app forward: the user is looking at the browser.
                cx.activate(true);
                window.activate_window();
                cx.notify();
                true
            }
        }
    }
}

impl Render for Desk {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = h_flex()
            .gap_2()
            // The family's mark: the Galata Tower.
            .child(
                gpui_kit::svg()
                    .path(crate::theme::LOGO)
                    .size(gpui_kit::px(18.))
                    .flex_shrink_0()
                    .text_color(Palette::text()),
            )
            .child(
                div()
                    .text_color(Palette::amber())
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child("galata"),
            )
            .child(div().text_color(Palette::muted()).child("desk"));

        let (switch, bar_end, body, footer) = match &self.screen {
            Screen::Connect(state) => (
                None,
                None,
                connect::render(state, cx).into_any_element(),
                None,
            ),
            Screen::Connected {
                section,
                portfolio,
                markets,
                ..
            } => {
                let switch = TabBar::new("sections")
                    .segmented()
                    .small()
                    .selected_index(match section {
                        Section::Portfolio => 0,
                        Section::Markets => 1,
                        Section::Funding => 2,
                    })
                    .on_click(cx.listener(|this, ix: &usize, window, cx| {
                        let to = match *ix {
                            0 => Section::Portfolio,
                            1 => Section::Markets,
                            _ => Section::Funding,
                        };
                        this.show(to, window, cx);
                    }))
                    .children(["Portfolio", "Markets", "Funding"]);
                let account = portfolio.update(cx, |view, cx| view.account_controls(cx));
                // The current section says how fresh it is; the account is
                // the same in both.
                let (status, body) = match (section, markets) {
                    (Section::Markets, Some(markets)) => (
                        markets.update(cx, |view, cx| view.status(cx)),
                        markets.clone().into_any_element(),
                    ),
                    // Funding is drawn from what the Portfolio view holds.
                    (Section::Funding, _) => (
                        portfolio.update(cx, |view, cx| view.freshness(cx)),
                        portfolio.read(cx).funding(),
                    ),
                    _ => (
                        portfolio.update(cx, |view, cx| view.freshness(cx)),
                        portfolio.clone().into_any_element(),
                    ),
                };
                let end = h_flex()
                    .gap_2()
                    .child(status)
                    .child(account)
                    .into_any_element();
                // The account in one line, under whichever section is shown.
                let footer = portfolio.read(cx).footer();
                (Some(switch), Some(end), body, Some(footer))
            }
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .child(h_flex().gap_4().child(title).children(switch))
                    .child(div().flex_1())
                    .children(bar_end),
            )
            .child(div().flex_1().min_h_0().child(body))
            .children(footer)
    }
}

#[cfg(test)]
impl Desk {
    /// The portfolio view, when an account is connected.
    pub fn portfolio_view(&self) -> Option<Entity<Portfolio>> {
        match &self.screen {
            Screen::Connected { portfolio, .. } => Some(portfolio.clone()),
            Screen::Connect(_) => None,
        }
    }

    /// The Markets view, once Markets has been opened.
    pub fn markets_view(&self) -> Option<Entity<Markets>> {
        match &self.screen {
            Screen::Connected { markets, .. } => markets.clone(),
            Screen::Connect(_) => None,
        }
    }

    /// Which section is shown, when an account is connected.
    pub fn section(&self) -> Option<Section> {
        match &self.screen {
            Screen::Connected { section, .. } => Some(*section),
            Screen::Connect(_) => None,
        }
    }
}
