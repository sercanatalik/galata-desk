<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-on-dark.svg">
  <img src="assets/logo.svg" alt="" width="72" align="right">
</picture>

# galata-desk

[![MIT](https://img.shields.io/badge/licence-MIT-blue.svg)](LICENSE-MIT)
[![Rust 1.98+](https://img.shields.io/badge/rust-1.98%2B-b7410e.svg)](rust-toolchain.toml)
[![Built with GPUI Kit](https://img.shields.io/badge/ui-GPUI%20Kit%200.10-000000.svg)](https://github.com/longbridge/gpui-kit)
[![macOS](https://img.shields.io/badge/platform-macOS-lightgrey.svg)](#quick-start)

**A native macOS desktop for one Hyperliquid account: portfolio, markets,
funding and history, with orders and positions you can manage from the same
window.**

You connect by signing a message in your browser wallet. Nothing is signed
for trading unless you save an API key and confirm an action.

> **Status: 0.x, macOS only.**

Part of the Galata trading framework, alongside
[galata-datawatch](https://github.com/sercanatalik/galata-datawatch),
[galata-tower](https://github.com/sercanatalik/galata-tower),
[galata-vault](https://github.com/sercanatalik/galata-vault) and
[galata-watcher](https://github.com/sercanatalik/galata-watcher). It depends
on none of them.

## Screenshots

The account shown is one of Hyperliquid's own public vaults.

| | |
|---|---|
| ![Portfolio: account value, breakdown, PNL chart and positions](assets/screenshots/portfolio.png) | ![Markets: the selected market's detail and candlestick chart above heatmap tiles](assets/screenshots/markets.png) |
| **Portfolio.** Summary, breakdown, PNL chart and positions with Close and TP/SL. | **Markets.** Market detail, a chart with your fills marked, and tiles coloured by change. |

<img src="assets/screenshots/connect.png" alt="The connect page in a browser" width="420">

**The connect page**, served by the app to your browser.

## Quick start

You need macOS and Rust (`rust-toolchain.toml` pins the version).

```sh
git clone https://github.com/sercanatalik/galata-desk
cd galata-desk
cargo run -p galata-desk
```

Press **Connect wallet**, pick a wallet in the page that opens and sign the
message. It is not a transaction and grants no permission to trade.

## Features

- **Portfolio**: account value, PNL, volume and drawdown by period, equity
  breakdown, a value/PNL chart, and tabs for positions, balances and open
  orders.
- **Markets**: a heatmap of up to 40 watched markets. Selecting one shows
  its details, a candlestick chart with your fills and entry price, and a
  position panel with PNL, last trade, open orders and alerts.
- **Funding**: what your positions pay and earn, current rates, and rate
  history.
- **History**: trades, orders, funding payments, deposits and withdrawals,
  with CSV export.
- **Watcher**: with [galata-watcher](https://github.com/sercanatalik/galata-watcher)
  connected, edit its call levels, stop alerts and liquidation distance.
  Levels are drawn on the chart.
- **Trading**: an order ticket (market or limit, size in USD or coin,
  optional TP/SL), cancel and amend orders, close positions and set TP/SL.

## Trading safeguards

- Trading needs an API wallet key, saved in **Settings** and kept in the
  macOS Keychain. Nothing is sent until you confirm.
- Mainnet trading also needs **Allow trading on mainnet**, off by default.
- Every action passes one gate in the core (`trade::gate`) that checks the
  network, the key and that the key is an approved API wallet of the account.
- Position closes are reduce-only. The app never retries an order on its own.

## Settings and storage

- **Use testnet**, **API key** (per network), **Allow trading on mainnet**,
  **Quick order sizes** and the **Watcher** address and token.
- Other state lives in `~/Library/Application Support/galata-desk/`. No key
  or seed phrase is ever written there.
- Phone wallets need a WalletConnect project id from
  <https://dashboard.reown.com>, set as `reown_project_id` in `config.toml`
  or `GALATA_DESK_REOWN_PROJECT_ID`.

## Development

```sh
cargo run -p galata-desk                                # the app
cargo test --workspace                                  # no network needed
cargo clippy --workspace --all-targets -- -D warnings
./scripts/bundle-macos.sh                               # target/bundle/Galata Desk.app
```

```text
crates/galata-desk-core/   no UI: venue client, figures, feeds, signing, store
crates/galata-desk/        the GPUI app
connect/                   the connect page, built to connect/dist (committed)
```

To render the window to a PNG without a screen, build with the `shot`
feature:

```sh
cargo build -p galata-desk --features shot
GALATA_DESK_SHOT=/tmp/desk.png GALATA_DESK_SHOT_STATE=section=markets,select=BTC \
  ./target/debug/galata-desk
```

## Not built yet

Leverage and margin changes, stop and TWAP entries, transfers, spot markets
in Markets, Windows and Linux.

## Licence

MIT. See [LICENSE-MIT](LICENSE-MIT). Embedded fonts (Instrument Sans,
JetBrains Mono) are under the SIL Open Font License.
