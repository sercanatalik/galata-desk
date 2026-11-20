//! The window: the connect screen until an account is connected, then its
//! portfolio.

use galata_desk_core::connect::{Attempt, AttemptEvent};
use galata_desk_core::network::Network;
use galata_desk_core::store::{Account, QuickSizes};
use gpui_kit::component::button::{Button, ButtonVariants};
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme, Icon, IconName, Sizable, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{Context, Entity, Subscription, TestSupportExt, Window, deferred, div};

use crate::Core;
use crate::connect::{self, ConnectState};
use crate::funding::Funding;
use crate::history::History;
use crate::markets::Markets;
use crate::portfolio::{Portfolio, PortfolioEvent};
use crate::settings::{Settings, SettingsEvent};
use crate::theme::Palette;
use crate::watcher::WatcherSection;
use crate::watcher_link::WatcherLink;
use galata_desk_core::secrets::WATCHER_TOKEN_NAME;

/// What is shown while an account is connected.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Section {
    Portfolio,
    Markets,
    Funding,
    History,
    /// Offered only while a watcher is connected.
    Watcher,
}

enum Screen {
    Connect(ConnectState),
    /// Both sections live for as long as the account is connected, so each
    /// keeps its selections and the portfolio keeps refreshing under Markets.
    Connected {
        section: Section,
        portfolio: Entity<Portfolio>,
        /// Created with the portfolio: it asks the venue nothing until a row
        /// is chosen, and keeps its choice while another section is shown.
        funding: Entity<Funding>,
        /// Created with the portfolio: each of its tabs asks the venue
        /// nothing until it is first shown.
        history: Entity<History>,
        /// Created, and its stream opened, the first time Markets is chosen.
        markets: Option<Entity<Markets>>,
        _events: [Subscription; 2],
    },
}

pub struct Desk {
    screen: Screen,
    /// The settings page, while it is open over the current screen. The
    /// screen underneath is kept, so closing settings returns to it as it was.
    settings: Option<(Entity<Settings>, Subscription)>,
    /// The connection to galata-watcher: held while an account is connected
    /// and a watcher is saved in settings, and never otherwise, so with
    /// nothing saved no request goes to any watcher.
    watcher: Option<Entity<WatcherLink>>,
    /// The Watcher section, for as long as there is a connection to show.
    watcher_section: Option<Entity<WatcherSection>>,
}

impl Desk {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // A remembered account opens straight to its portfolio.
        let screen = match Core::get(cx).store.account() {
            Some(account) => Self::portfolio(account, window, cx),
            None => Screen::Connect(ConnectState::Idle { notice: None }),
        };
        #[allow(unused_mut)]
        let mut this = Self {
            screen,
            settings: None,
            watcher: None,
            watcher_section: None,
        };
        this.reconnect_watcher(window, cx);
        #[cfg(feature = "shot")]
        if std::env::var("GALATA_DESK_SHOT_STATE")
            .unwrap_or_default()
            .split(',')
            .any(|kv| kv == "settings=1")
        {
            this.open_settings(window, cx);
        }
        this
    }

    /// Show the settings page over the current screen.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings.is_some() {
            return;
        }
        let view = cx.new(|cx| Settings::new(window, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            SettingsEvent::Network(network) => this.set_network(*network, window, cx),
            SettingsEvent::MainnetTrading(allowed) => this.set_mainnet_trading(*allowed, cx),
            SettingsEvent::QuickSizes(list) => this.set_quick_sizes(list.clone(), cx),
            SettingsEvent::Watcher(url) => this.set_watcher(url.clone(), window, cx),
            SettingsEvent::Done => this.close_settings(cx),
        });
        let link = self.watcher.clone();
        view.update(cx, |view, cx| view.set_link(link, cx));
        self.settings = Some((view, events));
        cx.notify();
    }

    /// Connect to this watcher from now on, or to none, and remember it.
    fn set_watcher(
        &mut self,
        url: Option<galata_desk_core::watcher::WatcherUrl>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        cx.global_mut::<Core>().watcher_url = url;
        let core = Core::get(cx);
        // A choice that could not be saved still applies until the app quits.
        let _ = core.store.save_settings(&core.settings());
        self.reconnect_watcher(window, cx);
    }

    /// Hold a connection to the saved watcher while an account is connected,
    /// a new one each time this is called, and none otherwise; and hand it to
    /// every view that shows the watcher.
    fn reconnect_watcher(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let core = Core::get(cx);
        let token = core.secrets.get(WATCHER_TOKEN_NAME).ok().flatten();
        let (link, section) = match (&self.screen, core.watcher_url.clone(), token) {
            (Screen::Connected { portfolio, .. }, Some(url), Some(token)) => {
                let link = WatcherLink::start(url, token, cx);
                let portfolio = portfolio.clone();
                let section = {
                    let link = link.clone();
                    cx.new(|cx| WatcherSection::new(link, portfolio, window, cx))
                };
                (Some(link), Some(section))
            }
            _ => (None, None),
        };
        self.set_watcher_link(link, section, cx);
    }

    /// Hand `link` to every view that shows the watcher. Without one, the
    /// Watcher section goes, and the window leaves it if it was shown.
    fn set_watcher_link(
        &mut self,
        link: Option<Entity<WatcherLink>>,
        section: Option<Entity<WatcherSection>>,
        cx: &mut Context<Self>,
    ) {
        self.watcher = link.clone();
        self.watcher_section = section;
        if self.watcher_section.is_none()
            && let Screen::Connected { section, .. } = &mut self.screen
            && *section == Section::Watcher
        {
            *section = Section::Portfolio;
        }
        if let Some((settings, _)) = &self.settings {
            let link = link.clone();
            settings.update(cx, |view, cx| view.set_link(link, cx));
        }
        if let Screen::Connected {
            markets: Some(markets),
            ..
        } = &self.screen
        {
            markets.update(cx, |view, cx| view.set_watcher(link, cx));
        }
        cx.notify();
    }

    pub fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings = None;
        // A key may have been saved or removed while settings was open.
        self.reconsider_trading(cx);
        cx.notify();
    }

    /// Allow or forbid order actions on the main network, and remember it.
    fn set_mainnet_trading(&mut self, allowed: bool, cx: &mut Context<Self>) {
        cx.global_mut::<Core>().allow_mainnet_trading = allowed;
        let core = Core::get(cx);
        let _ = core.store.save_settings(&core.settings());
        self.reconsider_trading(cx);
        cx.notify();
    }

    /// Use these quick sizes in the order ticket from now on, and remember
    /// them. A list that could not be saved still applies until the app quits.
    fn set_quick_sizes(&mut self, list: QuickSizes, cx: &mut Context<Self>) {
        cx.global_mut::<Core>().quick_sizes = list;
        let core = Core::get(cx);
        let _ = core.store.save_settings(&core.settings());
        // The page's preview shows the list in use.
        if let Some((settings, _)) = &self.settings {
            settings.update(cx, |_, cx| cx.notify());
        }
        cx.notify();
    }

    /// Have the portfolio look again at whether orders can be acted on.
    fn reconsider_trading(&mut self, cx: &mut Context<Self>) {
        if let Screen::Connected { portfolio, .. } = &self.screen {
            portfolio.update(cx, |view, cx| view.reconsider_trading(cx));
        }
    }

    /// Show another network from now on: remember the choice, and build the
    /// connected sections afresh, since each reads the venue's addresses and
    /// opens its connections when it is created.
    fn set_network(&mut self, network: Network, window: &mut Window, cx: &mut Context<Self>) {
        if Core::get(cx).network == network {
            return;
        }
        cx.global_mut::<Core>().network = network;
        // A choice that could not be saved still applies until the app quits.
        let core = Core::get(cx);
        let _ = core.store.save_settings(&core.settings());
        if matches!(self.screen, Screen::Connected { .. })
            && let Some(account) = Core::get(cx).store.account()
        {
            self.screen = Self::portfolio(account, window, cx);
            self.reconnect_watcher(window, cx);
        }
        // Each network has its own API key.
        if let Some((settings, _)) = &self.settings {
            settings.update(cx, |view, cx| view.reload(cx));
        }
        cx.notify();
    }

    fn portfolio(account: Account, window: &mut Window, cx: &mut Context<Self>) -> Screen {
        let address = account.address.as_str().to_string();
        let view = cx.new(|cx| Portfolio::new(account.address, window, cx));
        let events = cx.subscribe_in(&view, window, |this, _, event, window, cx| match event {
            PortfolioEvent::Disconnect => this.disconnect(cx),
            PortfolioEvent::OpenSettings => this.open_settings(window, cx),
        });
        // The window draws the portfolio's freshness and footer itself, in
        // every section, so it redraws when the portfolio changes.
        let changes = cx.observe(&view, |_, _, cx| cx.notify());
        let funding = cx.new(|cx| Funding::new(view.clone(), cx));
        let history = {
            let portfolio = view.clone();
            cx.new(|cx| History::new(portfolio, address, window, cx))
        };
        #[allow(unused_mut)]
        let mut screen = Screen::Connected {
            section: Section::Portfolio,
            portfolio: view,
            funding,
            history,
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
        #[cfg(feature = "shot")]
        if std::env::var("GALATA_DESK_SHOT_STATE")
            .unwrap_or_default()
            .split(',')
            .any(|kv| kv == "section=history")
        {
            Self::open_section(&mut screen, Section::History, window, cx);
        }
        screen
    }

    /// Show a section, creating the Markets view on its first use.
    fn open_section(screen: &mut Screen, to: Section, window: &mut Window, cx: &mut Context<Self>) {
        if let Screen::Connected {
            section,
            portfolio,
            funding,
            history,
            markets,
            ..
        } = screen
        {
            if to == Section::Markets && markets.is_none() {
                let portfolio = portfolio.clone();
                *markets = Some(cx.new(|cx| Markets::new(portfolio, window, cx)));
            }
            // The watchlist may have changed in Markets since Funding was
            // last shown.
            if to == Section::Funding {
                funding.update(cx, |view, cx| view.refresh_watchlist(cx));
            }
            // History loads and refreshes only while it is shown.
            if (to == Section::History) != (*section == Section::History) {
                history.update(cx, |view, cx| view.set_shown(to == Section::History, cx));
            }
            *section = to;
        }
    }

    pub fn show(&mut self, to: Section, window: &mut Window, cx: &mut Context<Self>) {
        if to == Section::Watcher && self.watcher_section.is_none() {
            return;
        }
        let opening_markets = matches!(&self.screen, Screen::Connected { markets: None, .. })
            && to == Section::Markets;
        Self::open_section(&mut self.screen, to, window, cx);
        // A Markets view made just now learns of the watcher.
        if opening_markets
            && let Screen::Connected {
                markets: Some(markets),
                ..
            } = &self.screen
        {
            let link = self.watcher.clone();
            markets.update(cx, |view, cx| view.set_watcher(link, cx));
        }
        cx.notify();
    }

    fn idle(&mut self, notice: Option<&str>, cx: &mut Context<Self>) {
        self.screen = Screen::Connect(ConnectState::Idle {
            notice: notice.map(|n| n.to_string().into()),
        });
        self.set_watcher_link(None, None, cx);
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
                self.reconnect_watcher(window, cx);
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
            .child(div().text_color(Palette::muted()).child("desk"))
            // Never leave it in doubt that this is not the real venue.
            .when(Core::get(cx).network.is_testnet(), |d| {
                d.child(
                    div()
                        .id("testnet-badge")
                        .ml_1()
                        .px_2()
                        .rounded(gpui_kit::px(5.))
                        .bg(Palette::amber().opacity(0.16))
                        .text_color(Palette::amber())
                        .text_size(gpui_kit::px(11.5))
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .child("TESTNET")
                        .test_support(),
                )
            });
        let settings_button = Button::new("settings")
            .small()
            .ghost()
            .icon(Icon::new(IconName::Settings))
            .tooltip("Settings")
            .on_click(cx.listener(|this, _, window, cx| {
                if this.settings.is_some() {
                    this.close_settings(cx);
                } else {
                    this.open_settings(window, cx);
                }
            }));

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
                funding,
                history,
                markets,
                ..
            } => {
                let mut names = vec!["Portfolio", "Markets", "Funding", "History"];
                if self.watcher_section.is_some() {
                    names.push("Watcher");
                }
                let switch = TabBar::new("sections")
                    .segmented()
                    .small()
                    .selected_index(match section {
                        Section::Portfolio => 0,
                        Section::Markets => 1,
                        Section::Funding => 2,
                        Section::History => 3,
                        Section::Watcher => 4,
                    })
                    .on_click(cx.listener(|this, ix: &usize, window, cx| {
                        let to = match *ix {
                            0 => Section::Portfolio,
                            1 => Section::Markets,
                            2 => Section::Funding,
                            3 => Section::History,
                            _ => Section::Watcher,
                        };
                        this.show(to, window, cx);
                    }))
                    .children(names);
                let account = portfolio.update(cx, |view, cx| view.account_controls(cx));
                // The current section says how fresh it is; the account is
                // the same in both.
                let (status, body) = match (section, markets) {
                    (Section::Markets, Some(markets)) => (
                        markets.update(cx, |view, cx| view.status(cx)),
                        // The order ticket opens from here and is the
                        // portfolio's dialog: draw it over the section.
                        div()
                            .relative()
                            .size_full()
                            .child(markets.clone())
                            .children(
                                portfolio
                                    .update(cx, |view, cx| view.dialog_layer(cx))
                                    .map(|dialog| deferred(dialog).with_priority(3)),
                            )
                            .into_any_element(),
                    ),
                    // Funding's positions and rates are the portfolio's.
                    (Section::Funding, _) => (
                        portfolio.update(cx, |view, cx| view.freshness(cx)),
                        funding.clone().into_any_element(),
                    ),
                    (Section::History, _) => (
                        portfolio.update(cx, |view, cx| view.freshness(cx)),
                        history.clone().into_any_element(),
                    ),
                    (Section::Watcher, _) if self.watcher_section.is_some() => (
                        portfolio.update(cx, |view, cx| view.freshness(cx)),
                        self.watcher_section.clone().unwrap().into_any_element(),
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

        // Settings is drawn over whichever screen is current, which stays as
        // it is underneath.
        let (switch, body) = match &self.settings {
            Some((settings, _)) => (None, settings.clone().into_any_element()),
            None => (switch, body),
        };

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .child(
                TitleBar::new()
                    .child(h_flex().gap_4().child(title).children(switch))
                    .child(div().flex_1())
                    .children(bar_end)
                    .child(settings_button),
            )
            .child(div().flex_1().min_h_0().child(body))
            .children(footer)
    }
}

#[cfg(test)]
impl Desk {
    /// The settings page, while it is open.
    pub fn settings_view(&self) -> Option<Entity<Settings>> {
        self.settings.as_ref().map(|(view, _)| view.clone())
    }

    /// The portfolio view, when an account is connected.
    pub fn portfolio_view(&self) -> Option<Entity<Portfolio>> {
        match &self.screen {
            Screen::Connected { portfolio, .. } => Some(portfolio.clone()),
            Screen::Connect(_) => None,
        }
    }

    /// The Funding view, when an account is connected.
    pub fn funding_view(&self) -> Option<Entity<Funding>> {
        match &self.screen {
            Screen::Connected { funding, .. } => Some(funding.clone()),
            Screen::Connect(_) => None,
        }
    }

    /// The History view, when an account is connected.
    pub fn history_view(&self) -> Option<Entity<History>> {
        match &self.screen {
            Screen::Connected { history, .. } => Some(history.clone()),
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

    /// The connection to the watcher, while there is one.
    pub fn watcher_link(&self) -> Option<Entity<WatcherLink>> {
        self.watcher.clone()
    }

    /// The Watcher section, while a watcher is connected.
    pub fn watcher_section(&self) -> Option<Entity<WatcherSection>> {
        self.watcher_section.clone()
    }

    /// Which section is shown, when an account is connected.
    pub fn section(&self) -> Option<Section> {
        match &self.screen {
            Screen::Connected { section, .. } => Some(*section),
            Screen::Connect(_) => None,
        }
    }
}
