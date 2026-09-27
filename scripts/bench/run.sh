#!/usr/bin/env bash
# run.sh [DIR] -- the performance checks A-P-1 to A-P-7 (design 11.4, 13.1).
#
# Release build without failpoints. DIR (default target/bench) holds the fixtures and must
# be on the btrfs filesystem the reference conditions name. The cross-filesystem target is
# an ext4 image created here unprivileged (mkfs.ext4 on a file), attached with
# `udisksctl loop-setup` and mounted with `udisksctl mount`; both are undone on exit.
#
# Prints one PASS or FAIL line per check, and appends the numbers and the conditions to
# docs/perf/history.md. Exit status: 0 when every check passes.
set -euo pipefail

top="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$top"
dir="$(realpath -m "${1:-target/bench}")"
mkdir -p "$dir"
export MC_BENCH_DIR="$dir"
hist="docs/perf/history.md"
declare -A result note
fails=0

say() { printf '%s\n' "$*" >&2; }
check() { # id pass(0/1) text
  if [ "$2" = 1 ]; then result[$1]=PASS; else result[$1]=FAIL; fails=$((fails + 1)); fi
  note[$1]="$3"
  printf '%s %s: %s\n' "${result[$1]}" "$1" "$3"
}
le() { awk -v a="$1" -v b="$2" 'BEGIN { exit !(a <= b) }'; }

# ---- conditions ----------------------------------------------------------------------------
ac="unknown"
for s in /sys/class/power_supply/*/online; do [ -r "$s" ] && ac="$([ "$(cat "$s")" = 1 ] && echo on || echo off)"; done
profile="$(powerprofilesctl get 2>/dev/null || cat /sys/firmware/acpi/platform_profile 2>/dev/null || echo unknown)"
gov="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
fs="$(stat -f -c %T "$dir")"
cond="AC $ac, power profile $profile, governor $gov, fixtures on $fs"
say "conditions: $cond"

# ---- build ---------------------------------------------------------------------------------
cargo build --release --quiet
cargo bench --no-run --quiet 2>/dev/null
bin="$top/target/release/manycommander"
driver() { cargo bench --quiet --bench driver -- "$@" 2>/dev/null; }

# ---- fixtures ------------------------------------------------------------------------------
src="$dir/src"
mkdir -p "$src"
make_files() { # dir count bytes
  [ -f "$1/.complete" ] && return
  mkdir -p "$1"
  say "fixture: $1 ($2 files of $3 bytes)"
  python3 - "$1" "$2" "$3" <<'PY'
import os, sys
d, n, size = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
data = os.urandom(size) if size else b""
for i in range(n):
    with open(os.path.join(d, f"f{i:06d}"), "wb") as f:
        f.write(data)
open(os.path.join(d, ".complete"), "w").close()
PY
}
make_files "$src/many" 100000 0
make_files "$src/k1a" 1000 0
make_files "$src/k1b" 1000 0
make_files "$src/small" 50000 4096
for g in 4 10; do
  f="$src/big${g}g"
  if [ ! -f "$f" ] || [ "$(stat -c %s "$f")" -ne $((g << 30)) ]; then
    say "fixture: $f"
    head -c "$((g << 30))" /dev/urandom > "$f"
  fi
done
sync

# ---- ext4 image ----------------------------------------------------------------------------
img="$dir/ext4.img"
loop="" mnt=""
cleanup() {
  [ -n "$mnt" ] && udisksctl unmount -b "$loop" >/dev/null 2>&1 || true
  [ -n "$loop" ] && udisksctl loop-delete -b "$loop" >/dev/null 2>&1 || true
}
trap cleanup EXIT
if [ ! -f "$img" ]; then
  truncate -s 40G "$img"
  mkfs.ext4 -q -F -L mcbench -E root_owner="$(id -u):$(id -g)" "$img"
fi
loop="$(udisksctl loop-setup -f "$img" | sed -n 's/.* as \(\/dev\/loop[0-9]*\)\..*/\1/p')"
[ -n "$loop" ] || { say "run.sh: udisksctl loop-setup failed"; exit 1; }
mnt="$(udisksctl mount -b "$loop" | sed -n 's/.* at \(.*\)$/\1/p' | sed 's/\.$//')"
[ -d "$mnt" ] || { say "run.sh: udisksctl mount failed"; exit 1; }
say "ext4 image: $loop at $mnt"
clean_ext4() { find "$mnt" -mindepth 1 -maxdepth 1 ! -name lost+found -exec rm -rf {} +; }

# ---- A-P-3, A-P-4: criterion -----------------------------------------------------------------
cargo bench --quiet --bench listing -- --noplot >/dev/null 2>&1
est() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['median']['point_estimate']/1e6)" "target/criterion/$1/new/estimates.json"; }
list_ms="$(est p3/list_and_sort)"; first_ms="$(est p3/first_batch)"
resort_ms="$(est p4/resort)"; filter_ms="$(est p4/filter)"
ok=0; le "$list_ms" 300 && le "$first_ms" 50 && ok=1
check A-P-3 $ok "$(printf '100k listed and sorted %.1f ms (<= 300), first batch %.1f ms (<= 50)' "$list_ms" "$first_ms")"
ok=0; le "$resort_ms" 30 && le "$filter_ms" 30 && ok=1
check A-P-4 $ok "$(printf 're-sort %.1f ms, filter %.1f ms (<= 30)' "$resort_ms" "$filter_ms")"

# ---- A-P-2 ---------------------------------------------------------------------------------
line="$(driver first-frame "$bin" "$src/k1a" "$src/k1b" 20)"
med="$(sed -n 's/.*median=\([0-9.]*\).*/\1/p' <<<"$line")"; max="$(sed -n 's/.*max=\([0-9.]*\).*/\1/p' <<<"$line")"
ok=0; le "$med" 50 && ok=1
check A-P-2 $ok "first full frame median $med ms, max $max ms over 20 starts (<= 50)"

# ---- A-P-1 ---------------------------------------------------------------------------------
idle_line="$(driver navigate "$bin" "$src" many 400)"
clean_ext4
copy_line="$(driver navigate "$bin" "$src" many 400 big10g "$mnt")"
clean_ext4
p_idle="$(sed -n 's/.*p99_ms=\([0-9.]*\).*/\1/p' <<<"$idle_line")"
p_copy="$(sed -n 's/.*p99_ms=\([0-9.]*\).*/\1/p' <<<"$copy_line")"
running="$(sed -n 's/.*job_running_after=\([a-z]*\).*/\1/p' <<<"$copy_line")"
ok=0; le "$p_idle" 16 && le "$p_copy" 16 && [ "$running" = true ] && ok=1
check A-P-1 $ok "p99 key-to-flush idle $p_idle ms, during a 10 GiB copy to ext4 $p_copy ms (<= 16); job still running after the samples: $running"

# ---- A-P-5 ---------------------------------------------------------------------------------
line="$(driver idle "$bin" "$src/k1a" 60)"
read -r sb sa tb ta < <(sed -n 's/.*switches_before=\([0-9]*\) switches_after=\([0-9]*\) ticks_before=\([0-9]*\) ticks_after=\([0-9]*\).*/\1 \2 \3 \4/p' <<<"$line")
ok=0; [ "$sb" = "$sa" ] && [ "$tb" = "$ta" ] && ok=1
check A-P-5 $ok "60 s idle: voluntary context switches $sb -> $sa, CPU ticks $tb -> $ta (unchanged)"

# ---- A-P-6 ---------------------------------------------------------------------------------
line="$(driver rss "$bin" "$src/many" "$src/many")"
rss="$(sed -n 's/.*rss_mb=\([0-9.]*\).*/\1/p' <<<"$line")"
ok=0; le "$rss" 40 && ok=1
check A-P-6 $ok "RSS $rss MB with both panels on 100k entries (<= 40)"

# ---- A-P-7 ---------------------------------------------------------------------------------
drv="$(cargo bench --no-run --bench driver --message-format=json 2>/dev/null | python3 -c 'import json,sys
for l in sys.stdin:
    try: m=json.loads(l)
    except Exception: continue
    if m.get("reason")=="compiler-artifact" and m.get("target",{}).get("name")=="driver" and m.get("executable"): print(m["executable"])' | tail -1)"
hf() { # name prepare cmd... -> mean seconds
  local out="$dir/hf-$1.json"; local prep="$2"; shift 2
  hyperfine --style none --warmup 1 --runs 3 --prepare "$prep" --export-json "$out" "$@" >/dev/null 2>&1
  python3 -c "import json,sys; print(' '.join(str(r['mean']) for r in json.load(open(sys.argv[1]))['results']))" "$out"
}
read -r cp_big mc_big < <(hf big "rm -rf $mnt/big4g; sync" "cp $src/big4g $mnt/" "$drv copy $src big4g $mnt")
read -r cp_small mc_small < <(hf small "rm -rf $mnt/small; sync" "cp -r $src/small $mnt/" "$drv copy $src small $mnt")
read -r mc_reflink < <(hf reflink "rm -f $dir/reflinked" "$drv copy $src big4g $dir/reflinked")
rm -f "$dir/reflinked"
# Moves consume their source: each run moves a fresh reflinked copy of the fixture.
prep_move="rm -rf $mnt/small $dir/movesrc; mkdir -p $dir/movesrc; cp -r --reflink=always $src/small $dir/movesrc/; sync"
read -r mv_small mcmv_small < <(hf move "$prep_move" "mv $dir/movesrc/small $mnt/" "$drv move $dir/movesrc small $mnt")
rm -rf "$dir/movesrc"; clean_ext4
r_big="$(awk -v a="$mc_big" -v b="$cp_big" 'BEGIN{printf "%.3f", a/b}')"
r_small="$(awk -v a="$mc_small" -v b="$cp_small" 'BEGIN{printf "%.3f", a/b}')"
r_move="$(awk -v a="$mcmv_small" -v b="$mv_small" 'BEGIN{printf "%.3f", a/b}')"
ok=0; le "$r_big" 1.10 && le "$r_small" 1.5 && le "$mc_reflink" 1.0 && le "$r_move" 2.0 && ok=1
check A-P-7 $ok "$(printf '4 GiB to ext4: %.2f s vs cp %.2f s (x%s, <= 1.10); 50k x 4 KiB: %.2f s vs cp -r %.2f s (x%s, <= 1.5); 4 GiB btrfs reflink copy %.3f s (< 1); move 50k x 4 KiB to ext4: %.2f s vs mv %.2f s (x%s, <= 2)' \
  "$mc_big" "$cp_big" "$r_big" "$mc_small" "$cp_small" "$r_small" "$mc_reflink" "$mcmv_small" "$mv_small" "$r_move")"

# ---- history -------------------------------------------------------------------------------
{
  printf '\n## %s\n\nConditions: %s. Commit %s%s.\n\n| Check | Result | Measurement |\n|---|---|---|\n' \
    "$(date '+%Y-%m-%d %H:%M')" "$cond" "$(git rev-parse --short HEAD)" "$(git diff --quiet HEAD -- src || echo ' (uncommitted changes in src)')"
  for id in A-P-1 A-P-2 A-P-3 A-P-4 A-P-5 A-P-6 A-P-7; do
    printf '| %s | %s | %s |\n' "$id" "${result[$id]}" "${note[$id]}"
  done
} >> "$hist"
say "appended to $hist"
[ "$fails" -eq 0 ]
