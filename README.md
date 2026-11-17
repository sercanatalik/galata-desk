<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/logo-on-dark.svg">
  <img src="assets/logo.svg" alt="" width="72" align="right">
</picture>

# galata-desk

**A native desktop view of a Hyperliquid account's portfolio.**

galata-desk shows what one Hyperliquid account is worth, how it has performed,
what it holds and what orders it has resting. It is read-only: it places no
order, moves no funds and holds no private key. It learns which account to
show by having you prove, with a wallet in your browser, that you own the
address.

It is the first trading-side screen of **Galata**, a low-latency algorithmic
trading framework, alongside
[galata-datawatch](https://github.com/sercanatalik/galata-datawatch),
[galata-tower](https://github.com/sercanatalik/galata-tower),
[galata-vault](https://github.com/sercanatalik/galata-vault) and
galata-research.

> **Status: 0.x.** macOS only. The portfolio screen and the connection flow
> are built; connecting with a real wallet has not yet been exercised end to end.

---

## What it shows

- **Account value**, 14-day volume, and taker and maker fee rates for
  perpetuals or spot.
- **A breakdown**: PNL, volume and max drawdown for 24 hours, 7 days, 30 days
  or all time, for the whole account or perpetuals only; and total, perpetuals,
  spot and vault equity, and the staking balance.
- **A history chart** of account value or PNL.
- **Positions** on every perpetuals dex, including builder-deployed ones;
  **balances**; and **open orders**.
- **Markets**: a live heatmap of the markets you watch (below).

Positions and balances refresh every 5 seconds, orders every 10, the rest
every minute. When Hyperliquid cannot be reached the last figures stay on
screen, marked with the time they are from. Gains are blue and losses orange,
and every signed amount carries its `+` or `-`.

Everything comes from Hyperliquid's public info API, which takes no signature.
The app sends nothing else.

### The footer

A line along the bottom of the window, under both sections, sums the account
up: **equity** (the same figure as Account value), **unrealised PNL** across
open positions and its share of equity, **PNL over 24 hours** and **all
time**, the number of open positions, the margin they use as a share of
equity, and when the figures were last refreshed. If Hyperliquid stops
answering, the figures stay and the time turns amber.

### Markets

A second section, beside Portfolio, shows the markets you watch as a grid of
tiles. Each tile has:

- the ticker, the builder dex it trades on (for example `xyz`), and an amber
  dot when you hold a position in it;
- the last price (the mid, or the mark where there is no mid) and the signed
  change over the period;
- the period's low and high, with a marker for where the last price sits
  between them;
- volatility over the period.

**Period** is 1 hour, 24 hours or 7 days, and is a rolling window ending now,
built from 1-minute, 15-minute or 2-hour candles. **Volatility** is the sample
standard deviation of the returns between those candles' closes, scaled to the
whole period. It is not annualised: `3.0%` over 24 hours reads as "it typically
moved about 3% in a day".

**Colour by** tints the tiles by change, by volatility, or by funding (blue
where longs pay, orange where shorts pay). The legend above the grid shows the
scale. The colour is strongest at a fixed bound: 1%, 6% and 15% change for the
three periods.

Point at a tile, or tab to it, for its detail: a sparkline of the period, mark
and oracle price, premium, funding per hour and as a yearly rate, 24-hour
volume, open interest, and your position if you have one.

**+ Add market** lists every perpetual market Hyperliquid has, on the main dex
and on builder dexes, with a search field. Remove a market with the `×` on its
tile, or with Delete while it is focused. The watchlist holds up to 40 markets
and is remembered between launches; the first time, it starts from the markets
you hold positions in.

Market data arrives over Hyperliquid's WebSocket and is live to within a
second or two; the title bar says **Live** or **Not live**. Candle history is
loaded once per market over REST, at a measured pace: the first five markets
at once, then about eleven a minute, so a full watchlist of 40 fills in over
about three minutes. If the connection drops, the last figures stay on
screen, marked with the time they are from, and the app reconnects by itself.

---

## Connecting an account

galata-desk never asks for a private key, a seed phrase or an API key. It
needs one thing: an address, and proof that it is yours.

1. Press **Connect wallet**. The app opens a page in your default browser. The
   page is served by the app itself, on `127.0.0.1`, for this one attempt.
2. Pick a wallet.
   - **Browser extensions** (Rabby, MetaMask, any wallet that announces itself
     to the page) are listed by name. Nothing needs configuring.
   - **Phone wallets** (MetaMask or Rabby on iOS, and others) connect by
     scanning a WalletConnect QR code. This needs a relay project id; see
     below.
3. The wallet asks you to sign a short plain-text message. Signing it proves
   you own the address. It is not a transaction, costs nothing, and grants no
   permission to trade or move funds. The message reads:

   ```text
   galata-desk wants to show the Hyperliquid portfolio of this account.

   Signing proves you own the address. It grants no permission to trade or move funds.

   Address: 0x...
   Nonce: <random, new for every attempt>
   Issued: <time>
   ```

4. The app checks the signature on your machine, shows the portfolio, and the
   page says you can close the tab.

An attempt that is not finished in five minutes expires. The app remembers
the address between launches and does not ask again until you disconnect.

Connect with the account that holds the funds. An API wallet (agent) address
has no portfolio of its own, and the venue answers for it with an empty one.

### Phone wallets: the relay project id

WalletConnect's relay requires a project id. It is free, and it is not a
secret: every site that uses WalletConnect ships its own in its page.

1. Create a project at <https://dashboard.reown.com>. No domain needs adding:
   pages on `127.0.0.1` are always permitted.
2. Give the id to galata-desk in either of two ways:

   ```toml
   # ~/Library/Application Support/galata-desk/config.toml
   reown_project_id = "your-project-id"
   ```

   ```sh
   export GALATA_DESK_REOWN_PROJECT_ID=your-project-id
   ```

   The environment variable wins when both are set.

Without an id the page still connects browser extensions, and says how to
enable phone wallets where the QR option would be.

### What is stored

One file, readable only by you:

```text
~/Library/Application Support/galata-desk/account.json
```

It holds the address and when it was proven. **Disconnect** deletes it.

---

## The app icon and a packaged app

The icon is the family's Galata Tower mark on a dark tile: `assets/icon.svg`,
with `assets/icon.png` and `assets/AppIcon.icns` rendered from it. The running
app sets it as its Dock icon, so it shows even under `cargo run`.

To build a double-clickable app with the icon in Finder and Launchpad:

```sh
./scripts/bundle-macos.sh        # writes target/bundle/Galata Desk.app
open "target/bundle/Galata Desk.app"
```

The bundle is signed ad hoc, which is enough to run it on the machine that
built it.

---

## Development

```sh
cargo run -p galata-desk                 # the app
cargo test --workspace                   # everything that needs no window

# One account's portfolio from the live venue, in the terminal:
cargo run -p galata-desk-core --example portfolio -- 0x<address>

# One connection attempt, without the app:
cargo run -p galata-desk-core --example connect

# Live market tiles in the terminal (1h, 24h or 7d; default 24h):
cargo run -p galata-desk-core --example markets -- --period 7d BTC xyz:NVDA
```

Development switches, all environment variables:

| Variable | Effect |
|---|---|
| `GALATA_DESK_DIR` | Use this directory instead of the application data directory. |
| `GALATA_DESK_INFO_URL` | Ask this info endpoint instead of Hyperliquid's. |
| `GALATA_DESK_WS_URL` | Use this WebSocket endpoint for market data instead of Hyperliquid's. |
| `GALATA_DESK_SHOT` | With `--features shot`: render the window offscreen to this PNG and quit. The window stays hidden. |
| `GALATA_DESK_SHOT_AFTER_MS` | How long to let the screen load first (default 2500). |
| `GALATA_DESK_SHOT_STATE` | Selections to open with, for example `tab=orders,period=week,series=value` or `section=markets,colour=funding,mopen=xyz:NVDA`. |
| `GALATA_DESK_SIZE` | Open the window at this size, for example `1100x700`. |

```sh
cargo run -p galata-desk --features shot   # with GALATA_DESK_SHOT=/tmp/desk.png
```

The connect page lives in `connect/` and its build output, `connect/dist`, is
committed: the Rust build embeds it and needs no Node toolchain. After
changing the page:

```sh
cd connect && pnpm install && pnpm build
```

`crates/galata-desk-core` links no UI toolkit and holds everything testable
without a window: the venue client, the portfolio's arithmetic, the refresh
schedule, the connection bridge and the proof check. `crates/galata-desk` is
the GPUI app on top of it.
