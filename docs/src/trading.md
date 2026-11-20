# Trading

Trading needs an API key: the private key of an API wallet you created on
Hyperliquid, saved in [Settings](settings.md). Nothing is sent until you
confirm, and each confirmation shows the network and the wallet that will
sign.

## Order ticket

**Buy / Long** and **Sell / Short** in the Markets section open the order
ticket. It is the one control in the app that can open or add to a position.

- **Market** fills at once at no worse than 5% from the mid, or not at all.
  **Limit** rests at your price (**Mid** fills in the mid).
- Size is entered in **USD** by default, or in the coin. Dollars become a
  coin size rounded down to the market's size step, and that coin size is
  what is sent. 25/50/75/100% fill in a share of what Hyperliquid says you
  can trade on that side.
- **Quick** buttons ($5k, $10k and $25k unless changed in Settings) fill in
  that many dollars of order value, not margin. A size above what you can
  trade is greyed out, and a line says the most you can trade.
- **Reduce only**, and an optional **take-profit / stop-loss** sent with the
  order and sized to it.
- The ticket shows what you can trade, your position, the leverage (set on
  Hyperliquid, not here), the order's value, margin and fee. It checks the
  size step, price precision, the $10 minimum, the maximum for that side and
  the TP/SL sides before sending. One line says exactly what will be sent.
- After you confirm, the ticket shows Hyperliquid's answer: filled with its
  average price, resting, refused with the reason, or unknown.

## Open orders

From the Portfolio **Open orders** tab, or a market's position panel:

- **Cancel** an order, or **Cancel all**. In the position panel, Cancel all
  covers only that market.
- **Amend** a limit order's price and size. The form checks price precision,
  size step and the $10 minimum before sending.

## Positions

From the Portfolio **Positions** tab, or a market's position panel. Every
order sent from here is reduce-only, so it can never add to a position or
reverse it.

- **Market** closes at once, at no worse than 5% from the mid. The dialog
  shows that worst price, and it is the price sent.
- **Limit** places a resting close at your price.
- Close part or all of a position, by size or 25/50/75/100%. A partial close
  must be worth at least $10. Closing the whole position is allowed at any
  value.
- **Close all** closes every position at market in one request.
- **TP/SL** sets one take-profit and one stop-loss per position. Both cover
  the whole position and close it at market when they trigger. Moving a level
  changes the order in place, so the position is never left without its stop.

## Safeguards

- Every action passes one gate in the core (`trade::gate`). It checks the
  network, the mainnet switch, that a key is saved, and that Hyperliquid
  lists the key as an approved API wallet of the connected account. Only then
  is the key read from the Keychain, used once and dropped.
- Mainnet trading also needs **Allow trading on mainnet**, which is off by
  default. Testnet needs no such switch.
- The app shows Hyperliquid's own answer. When none comes back, it says the
  outcome is unknown and reloads. It never retries on its own.
- The signing code is pinned to test vectors made with Hyperliquid's Python
  SDK.
