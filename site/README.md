# The site

A landing page, with the book under it at `/docs/`. It is built and checked
on every push but not published: GitHub Pages is off for the repository.

| File | What it is |
|---|---|
| `index.html` | The landing page: self-contained, inline CSS, no script. Every sentence on it comes from the README or the book. |
| `build.sh` | Builds everything into `_build/`. |
| `_build/` | Generated and ignored: `index.html`, `assets/` (logo, favicon, fonts, screenshots) and `docs/` (the book). |

The book itself stays in `../docs/`. `docs/book.toml` builds it into
`site/_build/docs`, and `docs/theme/galata.js` puts a link back to the landing
page at the top of its sidebar.

## Building it

Needs [mdBook](https://rust-lang.github.io/mdBook/) 0.5 on `PATH`.

```sh
site/build.sh            # writes site/_build; open site/_build/index.html
site/build.sh --serve    # the same, then previews the book with mdbook serve
```

`build.sh` refuses to build when a screenshot in `docs/src/images/` differs
from its namesake in `assets/screenshots/`. The latter is the source; the book
keeps copies because mdBook needs images inside `src`.

## Publishing it

`.github/workflows/pages.yml` runs `site/build.sh` on every push to `main` and
on pull requests, and uploads the built site as an artifact, which can be
downloaded from the run. It deploys only when run by hand. To publish at
`https://sercanatalik.github.io/galata-desk/`: turn Pages on with source
"GitHub Actions" (`gh api -X POST repos/sercanatalik/galata-desk/pages -f
build_type=workflow`), then run the workflow from the Actions tab.

## Its look

The app's own theme (`assets/themes/galata-dark.json`), dark only:

| Role | Value |
|---|---|
| Ground | `#0e1012` |
| Panel | `#15181b` |
| Raised | `#1b1f23` |
| Line | `#262b30` |
| Text | `#ece8df` |
| Muted | `#a29e94` |
| Brand and primary action | `#f0a73a` (hover `#f4b85c`), text on it `#0e1012` |

Instrument Sans for everything, JetBrains Mono for figures, commands and
eyebrow labels, both served from `assets/fonts/` (OFL), so the page makes no
request to another host. Headings are Instrument Sans SemiBold with tight
tracking. Radius 6 and 8, no shadows. The wordmark is "galata" in amber and
"desk" in muted, as the app's title bar sets it.
