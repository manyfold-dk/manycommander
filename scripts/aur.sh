#!/usr/bin/env bash
# aur.sh -- the AUR package manycommander-bin, made from a GitHub release.
#
#   render TAG DIR [PKGREL]
#           download the release tarball of TAG, check it against the release's .sha256
#           file, and write DIR/PKGBUILD from contrib/aur/manycommander-bin/PKGBUILD.in with
#           the version, the checksum and PKGREL (default 1) filled in
#   package DIR
#           namcap on DIR/PKGBUILD, makepkg, namcap on the package, then DIR/.SRCINFO. Arch
#           Linux only, and not as root: makepkg refuses root
#
# The aur workflow (.github/workflows/aur.yml) runs both in an Arch Linux container, installs
# and runs the package, and pushes DIR/PKGBUILD and DIR/.SRCINFO to the AUR. A namcap error
# fails package; a namcap warning is printed and passes.
set -euo pipefail

top="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
template="$top/contrib/aur/manycommander-bin/PKGBUILD.in"
releases="https://github.com/manyfold-dk/manycommander/releases/download"

need() { command -v "$1" >/dev/null 2>&1 || { echo "aur: $1 not found" >&2; exit 1; }; }
fail() { echo "aur: $*" >&2; exit 1; }

render() {
  local tag="${1:-}" dir="${2:-}" pkgrel="${3:-1}"
  [ -n "$tag" ] && [ -n "$dir" ] || fail "usage: scripts/aur.sh render TAG DIR [PKGREL]"
  # pkgver may not contain a hyphen, so a pre-release tag gets no AUR package.
  [[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail "$tag is not a release tag (vMAJOR.MINOR.PATCH)"
  [[ "$pkgrel" =~ ^[1-9][0-9]*$ ]] || fail "pkgrel $pkgrel is not a positive integer"
  need curl
  need sha256sum
  local version="${tag#v}" name tmp sum
  name="manycommander-$version-x86_64-linux.tar.gz"
  tmp="$(mktemp -d)"
  curl -fsSL --retry 3 -o "$tmp/$name" "$releases/$tag/$name"
  curl -fsSL --retry 3 -o "$tmp/$name.sha256" "$releases/$tag/$name.sha256"
  (cd "$tmp" && sha256sum -c --quiet "$name.sha256") || { rm -rf "$tmp"; fail "$name does not match its .sha256"; }
  sum="$(awk '{print $1; exit}' "$tmp/$name.sha256")"
  rm -rf "$tmp"
  [[ "$sum" =~ ^[0-9a-f]{64}$ ]] || fail "$name.sha256 holds no SHA-256"
  mkdir -p "$dir"
  sed -e "s/@PKGVER@/$version/" -e "s/@PKGREL@/$pkgrel/" -e "s/@SHA256@/$sum/" "$template" > "$dir/PKGBUILD"
  if grep -n '@[A-Z0-9]*@' "$dir/PKGBUILD"; then fail "$dir/PKGBUILD has a placeholder left"; fi
  echo "aur: $dir/PKGBUILD for $tag, pkgrel $pkgrel"
}

# namcap prints "<name> E: ..." for an error and "<name> W: ..." for a warning, and exits 0.
lint() {
  local out
  out="$(namcap "$1")"
  [ -z "$out" ] || printf '%s\n' "$out"
  if grep -q ' E: ' <<<"$out"; then fail "namcap reports an error in $1"; fi
}

package() {
  local dir="${1:-}"
  [ -n "$dir" ] || fail "usage: scripts/aur.sh package DIR"
  [ "$(id -u)" -ne 0 ] || fail "makepkg refuses root; run package as another user"
  need makepkg
  need namcap
  cd "$dir"
  lint PKGBUILD
  makepkg --cleanbuild --force --noconfirm
  local pkg
  for pkg in ./manycommander-bin-*.pkg.tar.zst; do lint "$pkg"; done
  makepkg --printsrcinfo > .SRCINFO
  echo "aur: $dir/.SRCINFO"
}

case "${1:-}" in
  render) shift; render "$@" ;;
  package) shift; package "$@" ;;
  *) echo "usage: scripts/aur.sh render TAG DIR [PKGREL] | package DIR" >&2; exit 2 ;;
esac
