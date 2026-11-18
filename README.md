<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-on-dark.svg">
  <img src="assets/logo.svg" alt="" width="72" align="right">
</picture>

# galata-desk

[![MIT](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE-MIT)
[![Rust 1.98+](https://img.shields.io/badge/rust-1.98%2B-b7410e.svg)](rust-toolchain.toml)
[![Built with GPUI Kit](https://img.shields.io/badge/ui-GPUI%20Kit%200.10-000000.svg)](https://github.com/longbridge/gpui-kit)
[![macOS](https://img.shields.io/badge/platform-macOS-lightgrey.svg)](#quick-start)

**A native macOS desktop for one Hyperliquid account: its portfolio, the
markets it trades, and the funding it pays, with orders and positions you can
manage from the same window.**

You prove you own an address by signing a message in your browser wallet.
From then on, the app reads public data. It signs nothing unless you save an
API key and confirm an action.

> **Status: 0.x, macOS only.** It can amend and cancel orders, close
> positions, and set their take-profit and stop-loss. It does not open or add
> to positions.

Galata is a Rust trading framework. Its siblings record market data
([galata-datawatch](https://github.com/sercanatalik/galata-datawatch)), watch
that record ([galata-tower](https://github.com/sercanatalik/galata-tower)) and
keep its secrets ([galata-vault](https://github.com/sercanatalik/galata-vault)).
galata-desk is the first screen on the trading side. It depends on none of
them.

---

## Screenshots

The account shown is one of Hyperliquid's own public vaults.

| | |
|---|---|
| ![Portfolio: account value, breakdown, PNL chart and positions](assets/screenshots/portfolio.png) | ![Markets: the selected market's detail and candlestick chart above heatmap tiles](assets/screenshots/markets.png) |
| **Portfolio.** Summary, breakdown, PNL chart, and the positions table with its Close and TP/SL controls. The footer is beneath. | **Markets.** The panel on a 1-hour chart with the account's fills marked B and S, above tiles coloured by change. |

<img src="assets/screenshots/connect.png" alt="The connect page in a browser" width="420">

**The connect page**, as the app serves it to your browser.

---

## Quick start

You need macOS and Rust. `rust-toolchain.toml` pins the version, and `rustup`
installs it for you.

```sh
git clone https://github.com/sercanatalik/galata-desk
cd galata-desk
cargo run -p galata-desk
```

Press **Connect wallet**, pick a wallet in the page that opens and sign the
message. The first build takes a few minutes.

---

## What it does

### Portfolio

- Account value, 14-day volume and fee rates.
- PNL, volume and max drawdown over 24 hours, 7 days, 30 days or all time.
- Equity by kind: perpetuals, spot, vaults and staking.
- A chart of account value or PNL.
- Tabs for **Positions** on every perpetuals dex (including builder dexes
  such as `xyz`), **Balances**, and **Open orders**.

### Markets

A heatmap of the markets you watch, up to 40.

- Each tile shows the last price, the change and volatility over the period
  you pick (3 minutes to 7 days), and a dot when you hold a position there.
- Tiles sort by change, volatility, funding or name, and are coloured by any
  of those.
- Selecting a tile opens a panel with that market's details and a
  candlestick chart. Your own fills are marked on it: **B** where you bought,
  **S** where you sold.

### Funding

- What your positions pay and earn per hour and per day at current rates,
  and what each has paid or earned since it opened.
- The current rates of the markets you watch.
- A chart of any of those markets' funding rate over 7, 30 or 90 days.

### Footer

Equity, unrealised and 24-hour PNL, the number of positions and the margin
they use, along the bottom of every screen.

Gains are blue and losses orange, and every signed figure carries a `+` or
`-`. While data is loading, placeholders stand in, never zeros. If
Hyperliquid stops answering, the last figures stay on screen with their time.

---

## Trading

Trading needs an API key: the private key of an API wallet you created on
Hyperliquid, saved in **Settings**. Nothing is sent until you confirm, and
each confirmation shows the network and the wallet that will sign.

**Open orders tab**

- **Cancel** an order, or **Cancel all**.
- **Amend** a limit order's price and size. The form checks Hyperliquid's
  rules before sending: price precision, size step and the $10 minimum.

**Positions tab**

Every order sent from this tab is reduce-only, so it can never add to a
position or reverse it.

- **Market** closes at once, at no worse than 5% from the mid price. The
  dialog shows that worst price, and it is the price sent.
- **Limit** places a resting close at your price.
- Close part or all of a position, by size or 25/50/75/100%. A partial close
  must be worth at least $10. Closing the whole position is allowed at any
  value.
- **Close all** closes every position at market in one request.
- **TP/SL** sets one take-profit and one stop-loss per position. Both cover
  the whole position and close it at market when they trigger. Moving a level
  changes the order in place, so the position is never left without its
  stop.

**Safeguards**

- Every action passes one gate in the core (`trade::gate`). It checks the
  network, the mainnet switch, that a key is saved, and that Hyperliquid
  lists the key as an approved API wallet of the connected account. Only then
  is the key read from the Keychain, used once and dropped.
- Mainnet trading also needs **Allow trading on mainnet**, which is off by
  default. Testnet needs no such switch.
- The app shows Hyperliquid's own answer. When none comes back, it says the
  outcome is unknown and reloads; it never retries on its own.
- The signing code is pinned to test vectors made with Hyperliquid's Python
  SDK.

---

## Connecting an account

galata-desk never asks for a seed phrase or a private key to connect.

1. **Connect wallet** opens a page the app serves on `127.0.0.1` for this one
   attempt.
2. Pick a browser-extension wallet (Rabby, MetaMask and others are found
   automatically). Phone wallets connect by WalletConnect QR code, which
   needs a relay project id (see below).
3. Sign a plain-text message. It is not a transaction, costs nothing, and
   grants no permission to trade.

The app checks the signature locally and remembers the address until you
**Disconnect**. Connect the account that holds the funds; an API wallet's
address has no portfolio of its own.

---

## Settings and storage

- **Use testnet** switches everything to Hyperliquid's test network. A
  **TESTNET** badge shows while it is on.
- **API key**: one per network, kept in the macOS Keychain under
  `dev.galata.desk`. The page shows the wallet address the key belongs to,
  never the key itself.
- **Allow trading on mainnet**: off by default.

Other state is in `~/Library/Application Support/galata-desk/`:
`account.json` (the connected address), `watchlist.json`, `settings.json`, and
an optional `config.toml`. No key or seed phrase is ever written there.

**Phone wallets** need a free WalletConnect project id from
<https://dashboard.reown.com>. Set it in `config.toml` as
`reown_project_id = "..."`, or in the `GALATA_DESK_REOWN_PROJECT_ID`
environment variable.

**Keychain prompts.** A development build is unsigned, so after a rebuild
macOS may ask for your password before the app can read a saved key. Choose
**Always Allow**.

---

## Development

```sh
cargo run -p galata-desk                                # the app
cargo test --workspace                                  # 481 tests, no network needed
cargo clippy --workspace --all-targets -- -D warnings

cargo run -p galata-desk-core --example portfolio -- 0x<address>   # a portfolio in the terminal
cargo run -p galata-desk-core --example markets -- BTC xyz:NVDA    # live tiles in the terminal
cargo run -p galata-desk-core --example signing                    # check signing against the venue
```

The repository has two crates and a web page:

```text
crates/galata-desk-core/   no UI: venue client, figures, feeds, signing, store
crates/galata-desk/        the GPUI app
connect/                   the connect page, built to connect/dist (committed)
```

Everything that can be tested without a window lives in the core. Its tests
run against recorded venue answers and an in-process WebSocket server. The
app's tests click through a headless window.

The app refreshes holdings every 5 seconds, orders every 10 and history every
60, and backs off when Hyperliquid asks it to. Market data comes over one
WebSocket connection. A full 40-market watchlist stays well inside the
venue's request budget.

After changing the connect page, rebuild it with
`cd connect && pnpm install && pnpm build`, and commit `connect/dist`.

**Environment switches** (none needed normally):

| Variable | Effect |
|---|---|
| `GALATA_DESK_DIR` | Use this data directory instead. |
| `GALATA_DESK_INFO_URL`, `GALATA_DESK_WS_URL` | Use other endpoints. Makes the app read-only. |
| `GALATA_DESK_SIZE` | Window size, for example `1100x700`. |

**Rendering to a file.** The `shot` feature renders the window offscreen to a
PNG and quits:

```sh
cargo build -p galata-desk --features shot
GALATA_DESK_SHOT=/tmp/desk.png GALATA_DESK_SHOT_AFTER_MS=5000 \
GALATA_DESK_SHOT_STATE=section=markets,select=xyz:NVDA ./target/debug/galata-desk
```

`GALATA_DESK_SHOT_STATE` picks the screen and state to render:

- `section=`, `select=`, `tab=` choose what is shown.
- `funding=`, `orders=` and `positions=` stage a dialog or a result. For
  example, `positions=market` opens the market-close dialog and
  `orders=amend` opens the amend form.

A `shot` build keeps no keys and uses a stand-in for the exchange, so it can
send nothing.

---

## A packaged app

```sh
./scripts/bundle-macos.sh                # writes target/bundle/Galata Desk.app
open "target/bundle/Galata Desk.app"
```

The bundle is signed ad hoc, which is enough to run it on the machine that
built it.

---

## Not built yet

- Opening or adding to positions, reversing them, trailing stops, and
  transfers.
- Trade history, the account's own funding payments, and TWAP.
- Spot markets in the Markets section, and alerts.
- Windows and Linux.
- Smart-contract wallets, which cannot prove ownership by signature.

---

## Licence

MIT. See [LICENSE-MIT](LICENSE-MIT). The embedded fonts, Instrument Sans and
JetBrains Mono, are under the SIL Open Font License (`assets/fonts/`).
