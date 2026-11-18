# Fixtures

Answers from Hyperliquid's public `POST /info`, recorded 2026-11-17 for the
venue's own HLP vault accounts and trimmed to a few rows each. They pin the
shapes the decoder reads.

Two are hand-written to the venue's documented shapes, because the recorded
accounts hold no spot token and no builder-dex position:
`spotClearinghouseState.json` and the positions in `clearinghouseState_xyz.json`.
The last entry of `userFills.json` is hand-written too, for a builder-dex fill.

`userFillsByTime.json` is three of the recorded `userFills` rows, oldest
first. The venue documents the same row shape for both queries. The loader
does not rely on the order: it sorts by time and drops repeats by `tid`.

`meta.json` and `meta_xyz.json` are the market lists out of the two
`metaAndAssetCtxs` fixtures, as the venue's `meta` query returns them.

`signing_vectors.json` is not a venue answer. It was made on 2026-11-18 with
the venue's Python SDK (`hyperliquid-python-sdk` 0.24.0): its `sign_l1_action`
and `action_hash`, with a fixed, public test key that guards nothing, for a
cancel of one order, a cancel of three, and a `batchModify`, each for mainnet
and testnet. Each entry holds the action, its MessagePack bytes, the action
hash, and the signature. The signer in `trade::sign` must reproduce all of it.

Five more actions were added the same day, with the same SDK version, key and
nonce. Each order was built with the SDK's `order_request_to_order_wire` and
`order_wires_to_order_action`, so the text of prices and sizes and the field
order are the SDK's own. They are:

- `market_close`: a reduce-only IOC sell on BTC.
- `limit_close`: a reduce-only GTC sell on a builder-dex asset (110001).
- `close_two_dexes`: one `order` action naming both of those assets.
- `position_tpsl`: a market take-profit and stop-loss with grouping
  `positionTpsl`.
- `modify_trigger`: a `batchModify` whose new order is a market stop-loss.

The MessagePack bytes are the SDK's `msgpack.packb` of the action.
The example `signing` makes the same check against the live venue.

`fundingHistory_BTC.json` and `fundingHistory_xyz_NVDA.json` were recorded on
2026-11-18: five hours of each market's funding, as `fundingHistory` returns
them. What else was learnt about that query the same day:

- **Rows.** One per hour, oldest first, stamped a few tens of milliseconds
  past the hour (`…00034`, `…00059`). A builder-dex market answers as a
  main-dex one does.
- **The range.** `startTime` and `endTime` are both included. A range holding
  more than 500 rows returns the oldest 500, so a page is asked as 500 hours
  ending one millisecond before the next page's start.
- **Nothing to return.** A range before the market was listed, or in the
  future, answers `[]`. A market the venue does not know answers `null`.
- **Weight.** The venue's documentation gives it weight 20 plus 1 for every
  20 rows returned, so a full page of 500 costs 45, against 1,200 a minute
  for the address. This is the documented figure. It was not measured by
  spending the allowance until the venue refused, because that would have
  stopped every other program on the same address for the rest of the
  minute. The answers carry no rate-limit header to read it from.

`allMids_xyz.json` was recorded from mainnet on 2026-11-18 with
`{"type": "allMids", "dex": "xyz"}`, trimmed to four markets. A builder dex's
mids are keyed with the dex's prefix (`xyz:NVDA`), unlike the main dex's.
The same request on testnet answered in the same shape. Without the `dex`
field, the answer holds only the main dex and spot.

`frontendOpenOrders_tpsl.json` comes from the `tpsl_probe` example, run by
the user on testnet on 2026-11-18 against a long `xyz:AMZN` position of
0.046 (asset 750013: `xyz` is dex 65 on testnet). It holds four trigger
orders:

- **Fixed-size TP and SL.** A take-profit and a stop-loss sent with grouping
  `positionTpsl` and the position's size (`s: "0.046"`). They are listed
  with `isPositionTpsl: false` and `sz: "0.046"`, as plain trigger orders.
- **Position-wide TP and SL.** The same pair sent with size `"0"`. They are
  listed with `isPositionTpsl: true` and `sz: "0.0"`, after the stop-loss was
  moved with `batchModify`.

What the same runs showed:

- A trigger order is answered `"waitingForTrigger"`.
- `batchModify` of a position stop-loss is accepted in one request and
  answered `{"resting": {"oid": <new>}}`: the stop gets a new order id.
- `orderType` is `Take Profit Market` or `Stop Market`, `tif` is null, and
  `triggerCondition` reads `Price above 400.28` / `Price below 240.16`.
- **A reduce-only order is held to the $10 minimum.** Closes of one size
  step ($0.32) were refused with "Order must have minimum value of $10."
  A limit close worth just over $10 rested.

A third probe run on testnet BTC (asset 3) the same day showed:

- One `order` action closing BTC and `xyz:AMZN` was filled for both.
- Closing the whole of a position worth $4.89 at market was allowed, so the
  $10 minimum binds only a partial close.
- A position TP/SL was gone once the position was closed.
