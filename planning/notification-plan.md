# notification-plan

**NOT PROPOSED.** Explored 2026-11-17, after the Funding section and tile
sorting landed. Nothing here is built. It records what was considered, what
was preferred and what is still unknown, so the work can be picked up without
repeating the discussion.

---

## Why

galata-desk now knows three things worth being told about, and all of them
need someone to be looking at the screen:

- a watched market moving more than a threshold over the chosen period;
- funding flipping sign on a position, or the account's net funding per day
  crossing a threshold;
- a position nearing its liquidation price.

The goal is for the app, or something beside it, to send a message when one
of these happens, including to a phone.

## The constraint that shapes everything

Rules evaluated by the desktop app only fire while the app is running and the
Mac is awake. A phone message matters most when the Mac is not in front of
you, which is when it tends to sleep. A keep-awake feature was built on
2026-11-17 and removed at the owner's request the same day, so it should not
be assumed.

So the first design question is **where the watching runs**, and the
notification channel comes second.

## Channels considered

| Channel | Setup | Who sees the alert | Verdict |
|---|---|---|---|
| macOS notification | None | Nobody | Do first. Only helps at the Mac. |
| ntfy, self-hosted | Container on the NAS, app on the phone | Own hardware (see iOS note) | **Preferred phone channel.** |
| ntfy, public server | Install app, pick a topic | ntfy.sh, in plain text | Fallback if the NAS cannot be reached from outside. |
| Telegram bot | Two minutes, no approval | Telegram | Good alternative; not chosen. |
| iMessage to self | macOS automation permission | Apple only | Unproven; see below. |
| WhatsApp (Meta Cloud API, Twilio) | Business account, approved templates, paid per message | Meta | Asked about first; too much setup for the value. |
| WhatsApp via an unofficial library | QR code | n/a | Against WhatsApp's terms. Do not build. |

Notes:

- A Mac notification is **not** mirrored to an iPhone on the same Apple
  account. Apple only does the reverse.
- iMessage-to-self (the app asks Messages to send to the owner's own Apple
  ID) would need no third party at all. It was not tested. Open points: how
  reliably Messages accepts scripted sends, and whether a message to yourself
  raises a notification on the phone.
- Channel facts above were written from memory on the date given and were not
  checked against current documentation. Verify before building, pricing and
  iOS behaviour in particular.

## Three shapes

**A. Desk posts straight to ntfy.**

```
  galata-desk (Mac) --HTTP--> ntfy (NAS) --> iPhone
```

Simplest. Needs the Mac awake. NATS is not involved.

**B. An alerts subject on NATS, with one bridge to ntfy.**

```
  galata-desk  ----+
  datawatch    ----+--> NATS alerts subject --> bridge (NAS) --> ntfy (NAS) --> iPhone
  vault, tower ----+
```

Any Galata service raises an alert the same way, and one place decides how
the owner is told. Desk-originated alerts still need the Mac awake; alerts
from always-on services do not. `galata-datawatch` already has the NATS
client (`crates/galata-broker`: subjects, identities, grants).

**C. The watching runs on the NAS.**

```
  Hyperliquid --> watcher (NAS, always on) --> NATS --> bridge --> ntfy --> iPhone
                        ^
                        |  rules
                  galata-desk (edits rules, shows alert history)
```

The only shape where a liquidation warning arrives with the Mac closed. The
desk edits rules instead of evaluating them. It is a new always-on service.
Market-move rules could read prices from NATS if datawatch publishes them
there; position and funding rules need the watcher to poll Hyperliquid for
the account, so the wallet address would live on the NAS (public data, no
keys).

## Preferred path

Build B so that it grows into C:

1. ntfy in a container on the Asustor, reachable from the phone.
2. An alerts subject on NATS and a small bridge that forwards it to ntfy.
3. galata-desk evaluates rules, publishes alerts to the subject, and also
   posts a macOS notification.
4. Later, move rule evaluation to a watcher on the NAS. Nothing downstream of
   the subject changes.

Step 2 is useful beyond the desk: datawatch's capture gaps and failures are
arguably the alerts most wanted on a phone.

## What the desk would need

- **Rules**, kept in a settings file: market move over the period, funding
  flip, net funding per day, distance to liquidation. Each with a threshold.
- **A cooldown per rule**, so a market oscillating around a threshold does
  not send twenty messages.
- **Lean alert text**: market and figure. No balances, and never the wallet
  address, in anything that leaves the machine.
- **A channel interface**, so macOS notification, ntfy and NATS are
  interchangeable ends.
- **A "send test" action** in settings.
- **Remembered settings.** Period, sort and colour-by reset on every launch
  today; an alert rule that resets on launch is useless, so the settings file
  should carry those too.
- The app holds no secrets today. An ntfy token or NATS credentials would be
  the first; decide where they are stored before building.

## Self-hosting ntfy: what is known and not

From memory, unverified:

- One small container with a config file and a data volume; images exist for
  Intel and ARM.
- Access can be restricted per topic with a login or token, so a guessable
  topic name is not a risk the way it is on the public server.
- Instant delivery on iPhone still involves ntfy's public service: the
  self-hosted server sends it a wake-up signal, and the phone then fetches
  the message from the self-hosted server. The alert text stays on own
  hardware; the fact that an alert happened does not.

## Open questions

1. **Can the iPhone reach the NAS away from home** (Tailscale, VPN, or a
   public address behind HTTPS)? If not, self-hosted ntfy only works at home.
   This was asked on 2026-11-17 and not answered.
2. **Is NATS on the Asustor the broker datawatch uses?** Datawatch's README
   describes its reference deployment as launchd services, which suggests a
   Mac. Not answered.
3. Does the ntfy iPhone app accept a plain-HTTP server on a private network,
   or require HTTPS? A ten-minute test.
4. Does iMessage-to-self work well enough to be a no-third-party channel? A
   ten-minute test.
5. Are alerts wanted away from the Mac at all? If alerts at the desk are
   enough, a macOS notification covers it and the rest of this plan is
   unnecessary.
6. Where does shape C's watcher live: a new Galata tree, or inside datawatch?

## First steps when this is picked up

- Answer open questions 1 and 5; they decide the shape.
- Run the two ten-minute tests (3 and 4).
- Then propose a change. Shape A or the desk half of B is a `galata-desk`
  change; the bridge and the watcher belong to another tree and need their
  own.
