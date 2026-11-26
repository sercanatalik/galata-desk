#!/usr/bin/env bash
# Build the site into site/_build (site/README.md):
#   _build/index.html  the landing page
#   _build/assets/     logo, favicon, fonts and screenshots
#   _build/docs/       the mdBook book (docs/book.toml builds it here)
# Needs mdbook on PATH. Pass --serve to preview the book locally.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/.." && pwd)"

# One copy of each screenshot is authoritative: assets/screenshots/. The
# book keeps copies in docs/src/images/ because mdBook needs them inside
# src; a copy that differs from its source is refused (design D5).
for book in "$repo"/docs/src/images/*.png; do
  source="$repo/assets/screenshots/$(basename "$book")"
  if [[ -f "$source" ]] && ! cmp -s "$source" "$book"; then
    echo "build.sh: docs/src/images/$(basename "$book") differs from assets/screenshots/$(basename "$book"); copy it again" >&2
    exit 1
  fi
done

rm -rf "$here/_build"
mdbook build "$repo/docs"
mkdir -p "$here/_build/assets/fonts" "$here/_build/assets/screenshots"
cp "$here/index.html" "$here/_build/index.html"
cp "$repo/assets/logo.svg" "$repo/assets/logo-on-dark.svg" "$here/_build/assets/"
cp "$repo/docs/theme/favicon.svg" "$repo/docs/theme/favicon.png" "$here/_build/assets/"
cp "$repo"/assets/fonts/*.ttf "$repo"/assets/fonts/OFL-*.txt "$here/_build/assets/fonts/"
cp "$repo"/assets/screenshots/*.png "$here/_build/assets/screenshots/"
echo "built $here/_build"

if [[ "${1:-}" == "--serve" ]]; then
  exec mdbook serve "$repo/docs" --open
fi
