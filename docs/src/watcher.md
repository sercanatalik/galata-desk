# Watcher

[galata-watcher](https://github.com/sercanatalik/galata-watcher) sends
Telegram alerts for the account while the desk is closed. Once it is
connected in [Settings](settings.md), the Watcher section shows and edits
what it alerts on. The desk never sends an alert itself.

From the top:

- **Call levels**: add, edit, delete and re-arm. A new level points away from
  the last price ("above" or "below") unless you choose. A level fires once,
  then waits to be re-armed.
- **Stop alerts**: one switch for take-profit and stop-loss alerts on every
  ticker. When on, each TP or SL order that fills sends an alert.
- **Near liquidation**: the distance, from 2% to 50%. Liquidation alerts
  can't be switched off.
- **Health**: one line at the bottom: when the watcher last checked the
  account, which account and network it watches, the configuration version
  and its last error. It turns amber once the watcher hasn't checked for
  three minutes.
- **Alert history**: hidden until you press "Alert history (N)" on the health
  line. It lists what the watcher sent, newest first, marked late or not
  delivered where that applies.

Levels can also be added from the [Markets](markets.md) chart, a tile, or the
position panel's bell.

## Conflicts and outages

Each change is saved with the configuration version it was based on. If the
configuration changed elsewhere in the meantime, nothing is overwritten: the
section shows the newer one and says the change wasn't saved.

While the watcher can't be reached, the section shows what it last read,
marked stale, and offers no edits.
