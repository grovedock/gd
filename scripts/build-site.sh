#!/bin/sh
# Assemble the GitHub Pages site: landing and pricing pages, brand and chart assets, mdBook docs.
# Usage: scripts/build-site.sh OUT_DIR   (needs `mdbook` on PATH, or MDBOOK=/path/to/mdbook)
set -eu
out="${1:?output directory}"
root="$(cd "$(dirname "$0")/.." && pwd)"
rm -rf "$out"
mkdir -p "$out/charts" "$out/fonts"

# Pages: fill {{KEY}} from site/site.conf (escaping sed's special characters in values).
set --
while IFS= read -r line; do
  case "$line" in ''|'#'*) continue ;; esac
  key="${line%%=*}"
  val="$(printf '%s' "${line#*=}" | sed 's/[&|\\]/\\&/g')"
  set -- "$@" -e "s|{{$key}}|$val|g"
done < "$root/site/site.conf"
for page in "$root"/site/*.html; do
  sed "$@" "$page" > "$out/$(basename "$page")"
done
if grep -n '{{' "$out"/*.html; then echo "build-site: unfilled {{KEY}} above; add it to site/site.conf" >&2; exit 1; fi

cp "$root/site/social-preview.png" "$root/site/site.css" "$out/"
# The custom domain: the site serves from the root of https://grovedock.com/.
printf 'grovedock.com\n' > "$out/CNAME"
cp "$root/docs/assets/brand/mark.svg" "$out/mark.svg"
cp "$root"/docs/assets/charts/*.svg "$out/charts/"
# Self-hosted fonts (OFL), from the console's pinned dependencies when installed.
fonts="$root/console/node_modules"
if [ -d "$fonts/@fontsource-variable/schibsted-grotesk" ]; then
  cp -L "$fonts/@fontsource-variable/schibsted-grotesk/files/schibsted-grotesk-latin-wght-normal.woff2" \
        "$fonts/@fontsource/ibm-plex-mono/files/ibm-plex-mono-latin-400-normal.woff2" "$out/fonts/"
  cp -L "$fonts/@fontsource-variable/schibsted-grotesk/LICENSE" "$out/fonts/OFL-schibsted-grotesk.txt"
  cp -L "$fonts/@fontsource/ibm-plex-mono/LICENSE" "$out/fonts/OFL-ibm-plex-mono.txt"
fi
"${MDBOOK:-mdbook}" build "$root/docs" -d "$out/docs"
