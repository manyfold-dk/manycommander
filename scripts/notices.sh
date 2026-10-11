#!/usr/bin/env bash
# notices.sh -- THIRD-PARTY-LICENSES, the licence texts of the crates compiled into the release
# binary (docs/adr/0032-third-party-licence-notices.md). The release workflow ships it in the
# tarball; CI writes it on every push, so a crate whose licence text cannot be found fails there
# first.
#
#   install DIR   download the pinned cargo-about for linux x86_64 into DIR, verified against
#                 CARGO_ABOUT_SHA256
#   generate OUT  write the notices for the dependency graph of Cargo.lock to OUT
#
# Tool version: contrib/notices/tools.env. CARGO_ABOUT names the binary (default: cargo-about on
# PATH). A missing tool fails the command: a check that cannot run has not passed.
set -euo pipefail

top="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
notices="$top/contrib/notices"
# shellcheck source=contrib/notices/tools.env
. "$notices/tools.env"
cargo_about="${CARGO_ABOUT:-cargo-about}"

install_tool() {
  local dir="${1:?usage: scripts/notices.sh install DIR}" tmp name
  name="cargo-about-$CARGO_ABOUT_VERSION-x86_64-unknown-linux-musl"
  mkdir -p "$dir"
  tmp="$(mktemp -d)"
  curl -sSfL --retry 3 -o "$tmp/$name.tar.gz" \
    "https://github.com/EmbarkStudios/cargo-about/releases/download/$CARGO_ABOUT_VERSION/$name.tar.gz"
  echo "$CARGO_ABOUT_SHA256  $tmp/$name.tar.gz" | sha256sum -c --quiet - \
    || { rm -rf "$tmp"; echo "notices: cargo-about checksum mismatch" >&2; exit 1; }
  tar -xzf "$tmp/$name.tar.gz" -C "$tmp" "$name/cargo-about"
  install -m 755 "$tmp/$name/cargo-about" "$dir/cargo-about"
  rm -rf "$tmp"
  echo "notices: cargo-about $CARGO_ABOUT_VERSION in $dir"
}

generate() {
  local out="${1:?usage: scripts/notices.sh generate OUT}"
  command -v "$cargo_about" >/dev/null 2>&1 \
    || { echo "notices: $cargo_about not found; run scripts/notices.sh install DIR" >&2; exit 1; }
  out="$(realpath -m "$out")"
  cd "$top"
  "$cargo_about" generate --locked --fail -c "$notices/about.toml" -o "$out" "$notices/notices.hbs"
  echo "notices: $(grep -c '^Used by:' "$out") licence texts in $out"
}

case "${1:-}" in
  install) install_tool "${2:-}" ;;
  generate) generate "${2:-}" ;;
  *) echo "usage: scripts/notices.sh [install DIR|generate OUT]" >&2; exit 2 ;;
esac
