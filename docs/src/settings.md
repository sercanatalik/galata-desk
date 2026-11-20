# Settings and storage

## Settings

- **Use testnet** switches everything to Hyperliquid's test network. A
  **TESTNET** badge shows while it is on.
- **API key**: one per network, kept in the macOS Keychain under
  `dev.galata.desk`. The page shows the wallet address the key belongs to,
  never the key itself.
- **Allow trading on mainnet**: off by default.
- **Quick order sizes**: one to four whole-dollar amounts, each at least $10,
  for the ticket's Quick buttons. The list is saved as you type once every
  amount is valid. **Reset** restores $5k, $10k and $25k.
- **Watcher**: galata-watcher's address and API token. With the watcher
  running on this Mac, the address is `http://127.0.0.1:8790` and the token
  is in `galata-watcher/var/secrets/api/api_token`. Plain `http://` is
  accepted only for this Mac, a home network or Tailscale; anything else must
  be `https://`. The token is kept in the Keychain and never shown again.
  **Test connection** says whether the watcher answers, and **Watch this
  account** points it at the connected account.

## Storage

Other state is in `~/Library/Application Support/galata-desk/`:

| File | Holds |
|---|---|
| `account.json` | The connected address. |
| `watchlist.json` | The watched markets. |
| `settings.json` | Network, mainnet switch, quick sizes, the watcher's address. |
| `config.toml` | Optional configuration. |

No key or seed phrase is ever written there.

## Phone wallets

Phone wallets need a free WalletConnect project id from
<https://dashboard.reown.com>. Set it in `config.toml` as
`reown_project_id = "..."`, or in the `GALATA_DESK_REOWN_PROJECT_ID`
environment variable.

## Keychain prompts

A development build is unsigned, so after a rebuild macOS may ask for your
password before the app can read a saved key. Choose **Always Allow**.
