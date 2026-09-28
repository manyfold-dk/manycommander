#!/usr/bin/env bash
# site.sh -- build, check and serve the manycommander.app site in site/.
#
#   build   zola build into site/public
#   check   build, then zola check: internal links and anchors (external links skipped; the
#           repository link answers 404 while the repository is private)
#   serve   zola serve with live reload on http://localhost:1111
#   install-zola DIR
#           download the pinned zola release for linux x86_64 into DIR, verified against
#           ZOLA_SHA256 (CI uses this; locally, any zola of that version on PATH will do)
#   worker  build, wrangler deploy --dry-run, then wrangler dev on 127.0.0.1:8788 and assert:
#           the apex answers 200 with the site/static/_headers headers, www answers 301 to
#           the apex with path and query kept, a missing page answers the 404 page
#
# Tool versions: site/tools.env, shared with .github/workflows/site.yml. ZOLA names the zola
# binary (default: zola on PATH). worker needs node (npx fetches the pinned wrangler). A
# missing tool fails the command: a check that cannot run has not passed.
set -euo pipefail

top="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
site="$top/site"
# shellcheck source=site/tools.env
. "$site/tools.env"
zola="${ZOLA:-zola}"

need() { command -v "$1" >/dev/null 2>&1 || { echo "site: $1 not found; $2" >&2; exit 1; }; }

zola_ok() {
  need "$zola" "install zola $ZOLA_VERSION (pacman -S zola, brew install zola, or mise) or set ZOLA"
  local have
  have="$("$zola" --version | awk '{print $2}')"
  [ "$have" = "$ZOLA_VERSION" ] || echo "site: warning: zola $have here, CI uses $ZOLA_VERSION" >&2
}

build() { zola_ok; (cd "$site" && "$zola" build); }

check() {
  build
  (cd "$site" && "$zola" check --skip-external-links)
}

install_zola() {
  local dir="${1:?usage: scripts/site.sh install-zola DIR}" tmp
  need curl "install curl"
  mkdir -p "$dir"
  tmp="$(mktemp)"
  curl -sSfL -o "$tmp" \
    "https://github.com/getzola/zola/releases/download/v$ZOLA_VERSION/zola-v$ZOLA_VERSION-x86_64-unknown-linux-gnu.tar.gz"
  echo "$ZOLA_SHA256  $tmp" | sha256sum -c --quiet - || { rm -f "$tmp"; echo "site: zola checksum mismatch" >&2; exit 1; }
  tar -xzf "$tmp" -C "$dir" zola
  rm -f "$tmp"
  echo "site: zola $ZOLA_VERSION in $dir"
}

serve() { zola_ok; cd "$site" && exec "$zola" serve --interface 127.0.0.1 --port 1111; }

# dev HOST PORT: starts wrangler dev in the background; the Worker sees request URLs on HOST
# (wrangler dev rewrites the URL to --host, so a Host header alone cannot select www).
dev() {
  "${wrangler[@]}" dev --ip 127.0.0.1 --port "$2" --host "$1" --show-interactive-dev-session=false \
    > "$logs/$1.log" 2>&1 &
  pids+=("$!")
  local _
  for _ in $(seq 1 60); do
    curl -s -o /dev/null "http://127.0.0.1:$2/" && return 0
    kill -0 "${pids[-1]}" 2>/dev/null || break
    sleep 0.5
  done
  cat "$logs/$1.log" >&2
  echo "site: wrangler dev for $1 did not start" >&2
  return 1
}

expect() { # expect DESCRIPTION ACTUAL WANTED
  if [ "$2" = "$3" ]; then echo "ok    $1"; else echo "FAIL  $1: got '$2', want '$3'"; fail=1; fi
}
head_of() { curl -s -o /dev/null -D - "$1" | tr -d '\r'; }
status() { sed -n 1p <<<"$1" | awk '{print $2}'; }
header() { grep -i "^$2:" <<<"$1" | cut -d' ' -f2-; }

worker() {
  need npx "install node"
  need curl "install curl"
  build
  wrangler=(npx --yes "wrangler@$WRANGLER_VERSION")
  export WRANGLER_SEND_METRICS=false
  cd "$site"
  "${wrangler[@]}" deploy --dry-run --outdir "$site/.wrangler/dry-run"

  logs="$(mktemp -d)"
  pids=()
  trap 'kill "${pids[@]}" 2>/dev/null; wait 2>/dev/null; rm -rf "$logs"' EXIT
  dev manycommander.app 8788
  dev www.manycommander.app 8789

  fail=0
  local apex=http://127.0.0.1:8788 www=http://127.0.0.1:8789 h
  h="$(head_of "$apex/")"
  expect "apex / status" "$(status "$h")" 200
  expect "apex nosniff" "$(header "$h" x-content-type-options)" nosniff
  expect "apex CSP" "$(header "$h" content-security-policy | cut -d';' -f1)" "default-src 'none'"
  expect "apex Referrer-Policy" "$(header "$h" referrer-policy)" strict-origin-when-cross-origin
  expect "apex Permissions-Policy present" "$(header "$h" permissions-policy | grep -c 'camera=()')" 1
  h="$(head_of "$apex/no-such-page/")"
  expect "missing page status" "$(status "$h")" 404
  expect "missing page is the 404 page" "$(curl -s "$apex/no-such-page/" | grep -c 'No such file or directory')" 1
  expect "missing page CSP" "$(header "$h" content-security-policy | cut -d';' -f1)" "default-src 'none'"
  expect "_headers is not served" "$(status "$(head_of "$apex/_headers")")" 404
  expect "font cache header" "$(header "$(head_of "$apex/fonts/jetbrains-mono-400.woff2")" cache-control)" \
    "public, max-age=31536000, immutable"
  h="$(head_of "$www/docs/keys/?from=test")"
  expect "www status" "$(status "$h")" 301
  expect "www Location" "$(header "$h" location)" "https://manycommander.app/docs/keys/?from=test"
  expect "www nosniff" "$(header "$h" x-content-type-options)" nosniff
  if [ "$fail" -ne 0 ]; then
    for f in "$logs"/*.log; do echo "--- $f" >&2; cat "$f" >&2; done
    return 1
  fi
  echo "site: worker checks passed"
}

case "${1:-check}" in
  build) build ;;
  check) check ;;
  serve) serve ;;
  worker) worker ;;
  install-zola) install_zola "${2:-}" ;;
  *) echo "usage: scripts/site.sh [build|check|serve|worker|install-zola DIR]" >&2; exit 2 ;;
esac
