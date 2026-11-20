# Watcher API answers

Recorded 2026-11-19 from `galata-watcher-api` (galata-watcher `40ca7a4` plus
its SIGTERM change), run against a temporary data folder. The configuration
is the example from galata-watcher's README, written with `PUT /config`. One
level was then marked fired, and two alerts with their deliveries, plus a
health record, were written by hand in the watcher's own formats. Every
address is the example `0x…aa`.

| File | Answer to |
|---|---|
| `config.json` | `GET /config`, 200, `ETag: "1"`: one fired and one armed level, a `stop_alerts` entry, a digest |
| `no_config.json` | `GET /config` before the first write, 404, `ETag: "0"` |
| `health.json` | `GET /health`, with a last error |
| `alerts.json` | `GET /alerts?since=0`: a late, undelivered stop-loss and a delivered level alert |
| `changed.json` | `PUT /config` with a stale `If-Match`, 412, `ETag: "1"` |
| `problems.json` | `PUT /config` with an invalid body, 422 |
