# Development

```sh
cargo run -p galata-desk                                # the app
cargo test --workspace                                  # no network needed
cargo clippy --workspace --all-targets -- -D warnings

cargo run -p galata-desk-core --example portfolio -- 0x<address>   # a portfolio in the terminal
cargo run -p galata-desk-core --example markets -- BTC xyz:NVDA    # live tiles in the terminal
cargo run -p galata-desk-core --example signing                    # check signing against the venue
```

## Layout

```text
crates/galata-desk-core/   no UI: venue client, figures, feeds, signing, store
crates/galata-desk/        the GPUI app
connect/                   the connect page, built to connect/dist (committed)
docs/                      this book (mdBook)
site/                      the landing page, and the build of both (site/README.md)
```

Everything that can be tested without a window lives in the core. Its tests
run against recorded venue answers and an in-process WebSocket server. The
app's tests click through a headless window.

After changing the connect page, rebuild it with
`cd connect && pnpm install && pnpm build`, and commit `connect/dist`.

## Data and polling

The app refreshes holdings every 5 seconds, orders every 10 and history every
60, and backs off when Hyperliquid asks it to. The History section asks for
nothing until a tab is opened. Market data comes over one WebSocket
connection. A full 40-market watchlist stays well inside the venue's request
budget.

## Environment switches

None are needed normally.

| Variable | Effect |
|---|---|
| `GALATA_DESK_DIR` | Use this data directory instead. |
| `GALATA_DESK_INFO_URL`, `GALATA_DESK_WS_URL` | Use other endpoints. Makes the app read-only. |
| `GALATA_DESK_SIZE` | Window size, for example `1100x700`. |

## Rendering to a file

The `shot` feature renders the window offscreen to a PNG and quits:

```sh
cargo build -p galata-desk --features shot
GALATA_DESK_SHOT=/tmp/desk.png GALATA_DESK_SHOT_AFTER_MS=5000 \
GALATA_DESK_SHOT_STATE=section=markets,select=xyz:NVDA ./target/debug/galata-desk
```

`GALATA_DESK_SHOT_STATE` picks the screen and state to render:

- `section=`, `select=`, `tab=` choose what is shown. In History,
  `htab=orders|funding|transfers`, `hgroup=0`, `hview=every`, `hperiod=all`
  choose its tab and views, and `hstale=1` shows its orders as not current.
- `funding=`, `orders=`, `positions=` and `ticket=` stage a dialog or a
  result. For example, `positions=market` opens the market-close dialog,
  `orders=amend` opens the amend form, and `ticket=tpsl` (with
  `section=markets`) opens the order ticket with a take-profit and stop-loss.

A `shot` build keeps no keys and uses a stand-in for the exchange, so it can
send nothing.

## A packaged app

```sh
./scripts/bundle-macos.sh                # writes target/bundle/Galata Desk.app
open "target/bundle/Galata Desk.app"
```

The bundle is signed ad hoc, which is enough to run it on the machine that
built it.

## This site

The docs are an [mdBook](https://rust-lang.github.io/mdBook/) in `docs/`,
built with the landing page into `site/_build` by `site/build.sh`; pass
`--serve` to preview the book. Every push to `main` builds the site in
GitHub Actions. It is not published: GitHub Pages is off for the repository,
and `site/README.md` says how to turn it on.
