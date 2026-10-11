#!/usr/bin/env bash
# check.sh -- the local verification gate (docs/adr/0010-local-first-verification-gate.md).
# quick, full and bench stack: each includes the one before. ci is full without MC_REQUIRE_ALL.
#
#   quick  format, lint, unit tests                                  during work
#   full   the documentation (warnings fail); every automated test   before every push
#          (skips fail), with and without the failpoints feature;
#          cargo-deny; the publication gate
#   ci     full without MC_REQUIRE_ALL; the publication gate checks  GitHub Actions
#          shapes and scanners only (--names none). A skipped test
#          prints SKIP and its reason; every test binary runs, so one
#          run reports every failure
#   bench  the benchmark harness (scripts/bench/run.sh)               performance changes
#
# Environment:
#   MC_XDEV_DIR     a directory on a different filesystem than target/ (default for full:
#                   /dev/shm/mc-xdev, tmpfs)
#   MC_REQUIRE_ALL  set to 1 by full: a test that would print SKIP fails instead
#   ESTATE_ROOT     the directory holding the estate-baseline checkout (default: the parent
#                   of this repository)
#   The publication gate reads MC_PUBLISH_NAMES (the path of the private name list) from
#   the environment or from the untracked file .publish-gate.confidential.env. Without it,
#   full fails: a gate that cannot run has not passed.
set -euo pipefail

tier="${1:-quick}"
top="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$top"

step() { printf '\n== %s\n' "$*"; }

# Temporary paths to remove however the script ends.
cleanup=()
trap 'rm -rf "${cleanup[@]}"' EXIT

quick() {
  step "cargo fmt --check"
  cargo fmt --all --check
  step "cargo clippy (all targets, all features)"
  cargo clippy --all-targets --all-features --quiet -- -D warnings
  step "cargo clippy (all targets, default features)"
  cargo clippy --all-targets --quiet -- -D warnings
  step "cargo test --lib"
  cargo test --lib --quiet
}

# The library's documentation with its private items: a broken or ambiguous intra-doc link,
# or a link from a public item to a private one, fails.
docs() {
  step "cargo doc (private items, all features, warnings fail)"
  RUSTDOCFLAGS="-D warnings" cargo doc --no-deps --document-private-items --all-features --quiet
}

# The tree that would be committed: tracked files plus untracked files that are not ignored.
export_tree() {
  local tmp="$1"
  git ls-files -z --cached --others --exclude-standard \
    | while IFS= read -r -d '' f; do [ -e "$f" ] || [ -L "$f" ] && printf '%s\0' "$f"; done \
    | xargs -0 -r cp --parents -P -t "$tmp"
}

run_publish_gate() {
  local names="$1" root gate tmp rc
  root="${ESTATE_ROOT:-$(dirname "$top")}"
  gate="$root/estate-baseline/scripts/publish-check/publish-check.sh"
  [ -x "$gate" ] || { echo "check: publication gate not found at $gate" >&2; return 1; }
  tmp="$(mktemp -d)"
  cleanup+=("$tmp" "$tmp.log")
  export_tree "$tmp"
  rc=0
  "$gate" "$tmp" --names "$names" --allow .publish-allow.tsv > "$tmp.log" 2>&1 || rc=$?
  grep -vE '^(==|trufflehog:)' "$tmp.log" | grep -v '^deny-list: 0 hit' || true
  rm -rf "$tmp" "$tmp.log"
  [ "$rc" -eq 0 ] || { echo "check: publication gate failed (rc=$rc)" >&2; return 1; }
  echo "publication gate: OK"
}

publication_gate() {
  step "publication gate"
  local names
  if [ -z "${MC_PUBLISH_NAMES:-}" ] && [ -f .publish-gate.confidential.env ]; then
    # shellcheck disable=SC1091
    . ./.publish-gate.confidential.env
  fi
  names="${MC_PUBLISH_NAMES:-}"
  [ -n "$names" ] && [ -f "$names" ] || { echo "check: MC_PUBLISH_NAMES is not set to a readable file; the gate cannot run" >&2; return 1; }
  run_publish_gate "$names"
}

# Public CI cannot carry the name list. Shapes and the two scanners still run.
publication_gate_shapes() {
  step "publication gate (shapes and scanners; no name list)"
  run_publish_gate none
}

full() {
  quick
  docs
  export MC_REQUIRE_ALL=1
  export MC_XDEV_DIR="${MC_XDEV_DIR:-/dev/shm/mc-xdev}"
  mkdir -p "$MC_XDEV_DIR" "$top/target/test-tmp"
  step "cargo test --all-targets (MC_REQUIRE_ALL=1, MC_XDEV_DIR=$MC_XDEV_DIR)"
  cargo test --all-targets --quiet
  step "cargo test --all-targets --features failpoints"
  cargo test --all-targets --features failpoints --quiet
  step "evidence"
  if unshare -rm true 2>/dev/null; then echo "unshare -rm true: ok"; else echo "unshare -rm true: FAILED"; fi
  local binds
  binds="$(cargo test --all-targets --features failpoints --quiet -- --list 2>/dev/null | grep -c 'bind_mount.*: test$' || true)"
  echo "bind-mount tests run (skips fail under MC_REQUIRE_ALL=1): $binds"
  step "cargo deny check"
  cargo deny --log-level error check
  publication_gate
}

ci() {
  unset MC_REQUIRE_ALL
  step "Cargo.lock is current"
  cargo metadata --locked --format-version 1 > /dev/null
  quick
  docs
  export MC_XDEV_DIR="${MC_XDEV_DIR:-/dev/shm/mc-xdev}"
  mkdir -p "$MC_XDEV_DIR" "$top/target/test-tmp"
  step "cargo test --all-targets (skips allowed, MC_XDEV_DIR=$MC_XDEV_DIR)"
  cargo test --all-targets --no-fail-fast -- --nocapture
  step "cargo test --all-targets --features failpoints (skips allowed)"
  cargo test --all-targets --no-fail-fast --features failpoints -- --nocapture
  step "cargo deny check"
  cargo deny --log-level error check
  publication_gate_shapes
}

bench() {
  full
  step "benchmarks"
  if [ -x scripts/bench/run.sh ]; then scripts/bench/run.sh; else echo "check: scripts/bench/run.sh missing" >&2; return 1; fi
}

case "$tier" in
  quick) quick ;;
  full) full ;;
  ci) ci ;;
  bench) bench ;;
  *) echo "usage: scripts/check.sh [quick|full|ci|bench]" >&2; exit 2 ;;
esac
printf '\ncheck.sh %s: PASS\n' "$tier"
