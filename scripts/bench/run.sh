#!/usr/bin/env bash
# run.sh [DIR] -- the performance checks: A-P-1 to A-P-8 (design 11.4, 13.1) and the phase 2
# checks A-QF-3, A-CD-3, A-MR-7, A-DJ-5, A-FD-5, A-FD-6, A-SP-2, A-HL-4 and P-6b (P2 11,
# 12), with the UI thread's share of Ctrl+R in a results tab (P-1/Ctrl+R), of a completed
# refresh of 100k entries (P-1/refresh), and the RSS after repeated Ctrl+R (RSS/Ctrl+R).
#
# Release build without failpoints. DIR (default target/bench) holds the fixtures and must
# be on the btrfs filesystem the reference conditions name. The cross-filesystem target is
# an ext4 image created here unprivileged (mkfs.ext4 on a file), attached with
# `udisksctl loop-setup` and mounted with `udisksctl mount`; both are undone on exit.
# A-SP-2 copies to tmpfs (/dev/shm). The M1 fixtures stay in DIR for the next run; the
# phase 2 fixtures (DIR/p2) are removed on exit unless MC_BENCH_KEEP=1.
#
# Prints one PASS or FAIL line per check, and appends the numbers and the conditions to
# docs/perf/history.md (not with MC_BENCH_HISTORY=0). ONLY="A-P-1 A-FD-5" runs a subset.
# Exit status: 0 when every check passes.
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
# a <= b; a missing or non-numeric value is a failure, never a pass.
le() { [[ "$1" =~ ^[0-9]+(\.[0-9]+)?$ ]] && awk -v a="$1" -v b="$2" 'BEGIN { exit !(a <= b) }'; }
# field=value of a driver line.
field() { sed -n "s/.*[ _]$1=\([0-9.a-zA-Z]*\).*/\1/p" <<<" $2" | head -1; }
ratio() { awk -v a="$1" -v b="$2" 'BEGIN { if (b > 0) printf "%.3f", a/b; else print "error" }'; }
ms() { awk -v s="$1" 'BEGIN { printf "%.1f", s * 1000 }'; }
# ONLY="A-P-1 A-P-7" runs a subset.
want() { [ -z "${ONLY:-}" ] || [[ " $ONLY " == *" $1 "* ]]; }

# ---- conditions ----------------------------------------------------------------------------
ac="unknown"
for s in /sys/class/power_supply/*/online; do [ -r "$s" ] && ac="$([ "$(cat "$s")" = 1 ] && echo on || echo off)"; done
profile="$(powerprofilesctl get 2>/dev/null || cat /sys/firmware/acpi/platform_profile 2>/dev/null || echo unknown)"
gov="$(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor 2>/dev/null || echo unknown)"
fs="$(stat -f -c %T "$dir")"
# Tool versions as major.minor only (the repository is public).
mm() { "$1" --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+' | head -1; }
cond="AC $ac, power profile $profile, governor $gov, fixtures on $fs, $(nproc) CPUs, 1-minute load $(cut -d' ' -f1 /proc/loadavg) at the start"
cond="$cond, fd $(mm fd), rg $(mm rg), hyperfine $(mm hyperfine)"
[ -n "${MC_BENCH_TABS:-}" ] && cond="$cond, $MC_BENCH_TABS tabs per panel"
say "conditions: $cond"

# ---- build ---------------------------------------------------------------------------------
cargo build --release --quiet
cargo bench --no-run --quiet 2>/dev/null
bin="$top/target/release/manycommander"
drv="$(cargo bench --no-run --bench driver --message-format=json 2>/dev/null | python3 -c 'import json,sys
for l in sys.stdin:
    try: m=json.loads(l)
    except Exception: continue
    if m.get("reason")=="compiler-artifact" and m.get("target",{}).get("name")=="driver" and m.get("executable"): print(m["executable"])' | tail -1)"
[ -x "$drv" ] || { say "run.sh: the benchmark driver was not built"; exit 1; }
# A failing driver run prints its error and yields "error", so the check fails and the
# run goes on.
driver() { "$drv" "$@" 2>"$dir/driver.err" || { sed -n '1,40p' "$dir/driver.err" >&2; echo error; }; }
# The find engine's worker count (P2 5.3), given to fd and rg too.
workers=$(( $(nproc) < 8 ? $(nproc) : 8 ))

# ---- fixtures ------------------------------------------------------------------------------
src="$dir/src"
mkdir -p "$src"
mkdir -p "$dir/markers"
make_files() { # dir count bytes; completion markers live in their own directory
  [ -f "$dir/markers/$(basename "$1")" ] && return
  mkdir -p "$1"
  say "fixture: $1 ($2 files of $3 bytes)"
  python3 - "$1" "$2" "$3" "$dir/markers/$(basename "$1")" <<'PY'
import os, sys
d, n, size, marker = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), sys.argv[4]
data = os.urandom(size) if size else b""
for i in range(n):
    with open(os.path.join(d, f"f{i:06d}"), "wb") as f:
        f.write(data)
open(marker, "w").close()
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
# Phase 2 fixtures: made by the driver (benches/common/mod.rs), once.
p2="$dir/p2"
fixture() { "$drv" fixture "$1"; }
sync

# ---- ext4 image, tmpfs, cleanup --------------------------------------------------------------
img="$dir/ext4.img"
loop="" mnt=""
shm="/dev/shm/mc-bench-$$"
# --no-user-interaction: a udisks action that needs authorization fails instead of raising
# a password prompt on the desktop.
ud() { udisksctl "$@" --no-user-interaction; }
cleanup() {
  rm -rf "$shm"
  [ "${MC_BENCH_KEEP:-0}" = 1 ] || rm -rf "$p2"
  [ -n "$loop" ] || return 0
  ud unmount -b "$loop" >/dev/null 2>&1 || true
  # loop-setup sets autoclear: the device detaches once it is unmounted.
  for _ in $(seq 20); do losetup "$loop" >/dev/null 2>&1 || return 0; sleep 0.1; done
  ud loop-delete -b "$loop" >/dev/null 2>&1 || say "run.sh: $loop is still attached; detach it with udisksctl loop-delete -b $loop"
}
trap cleanup EXIT
if want A-P-1 || want A-P-7 || want A-HL-4; then
if [ ! -f "$img" ]; then
  truncate -s 40G "$img"
  mkfs.ext4 -q -F -L mcbench -E root_owner="$(id -u):$(id -g)" "$img"
fi
loop="$(ud loop-setup -f "$img" | sed -n 's/.* as \(\/dev\/loop[0-9]*\)\..*/\1/p')"
[ -n "$loop" ] || { say "run.sh: udisksctl loop-setup failed"; exit 1; }
# A desktop automounter may mount the new device itself.
for _ in $(seq 30); do mnt="$(findmnt -n -o TARGET -S "$loop" || true)"; [ -n "$mnt" ] && break; sleep 0.1; done
if [ -z "$mnt" ]; then
  mnt="$(ud mount -b "$loop" | sed -n 's/.* at \(.*\)$/\1/p' | sed 's/\.$//')"
fi
[ -d "$mnt" ] || { say "run.sh: the ext4 image could not be mounted"; exit 1; }
say "ext4 image: $loop at $mnt"
fi
clean_ext4() { find "$mnt" -mindepth 1 -maxdepth 1 ! -name lost+found -exec rm -rf {} +; }

# ---- hyperfine -------------------------------------------------------------------------------
hf() { # name prepare cmd... -> mean seconds
  local out="$dir/hf-$1.json"; local prep="$2"; shift 2
  hyperfine --style none --warmup 1 --runs 3 --prepare "$prep" --export-json "$out" "$@" >/dev/null 2>&1
  python3 -c "import json,sys; print(' '.join(str(r['mean']) for r in json.load(open(sys.argv[1]))['results']))" "$out"
}
hf_median() {
  python3 -c "import json,sys; print(' '.join(str(r['median']) for r in json.load(open(sys.argv[1]))['results']))" "$1"
}
hfn() { # name cmd... -> median seconds; fast commands, run without a shell, warm cache
  local out="$dir/hf-$1.json"; shift
  hyperfine -N --style none --warmup 3 --min-runs 20 --export-json "$out" "$@" >/dev/null 2>&1 || { echo error; return; }
  hf_median "$out"
}
hfp() { # name prepare runs cmd... -> median seconds
  local out="$dir/hf-$1.json" prep="$2" runs="$3"; shift 3
  hyperfine --style none --warmup 1 --runs "$runs" --prepare "$prep" --export-json "$out" "$@" >/dev/null 2>&1 || { echo error; return; }
  hf_median "$out"
}

# ---- criterion -------------------------------------------------------------------------------
est() { python3 -c "import json,sys; print(json.load(open(sys.argv[1]))['median']['point_estimate']/1e6)" "target/criterion/$1/new/estimates.json" 2>/dev/null || echo error; }

# ---- A-P-3, A-P-4: criterion -----------------------------------------------------------------
if want A-P-3 || want A-P-4; then
cargo bench --quiet --bench listing -- --noplot >/dev/null 2>&1
list_ms="$(est p3/list_and_sort)"; first_ms="$(est p3/first_batch)"
resort_ms="$(est p4/resort)"; filter_ms="$(est p4/filter)"
ok=0; le "$list_ms" 300 && le "$first_ms" 50 && ok=1
check A-P-3 $ok "$(printf '100k listed and sorted %.1f ms (<= 300), first batch %.1f ms (<= 50)' "$list_ms" "$first_ms")"
ok=0; le "$resort_ms" 30 && le "$filter_ms" 30 && ok=1
check A-P-4 $ok "$(printf 're-sort %.1f ms, filter %.1f ms (<= 30)' "$resort_ms" "$filter_ms")"
fi

# ---- A-P-2 ---------------------------------------------------------------------------------
ff_med=""
if want A-P-2; then
line="$(driver first-frame "$bin" "$src/k1a" "$src/k1b" 20)"
med="$(sed -n 's/.*median=\([0-9.]*\).*/\1/p' <<<"$line")"; max="$(sed -n 's/.*max=\([0-9.]*\).*/\1/p' <<<"$line")"
ff_med="$med"
ok=0; le "$med" 50 && ok=1
check A-P-2 $ok "first full frame median $med ms, max $max ms over 20 starts (<= 50)"
fi

# ---- A-P-1 ---------------------------------------------------------------------------------
if want A-P-1; then
idle_line="$(driver navigate "$bin" "$src" many 400)"
clean_ext4
copy_line="$(driver navigate "$bin" "$src" many 400 big10g "$mnt")"
clean_ext4
p_idle="$(sed -n 's/.*p99_ms=\([0-9.]*\).*/\1/p' <<<"$idle_line")"
p_copy="$(sed -n 's/.*p99_ms=\([0-9.]*\).*/\1/p' <<<"$copy_line")"
running="$(sed -n 's/.*job_running_after=\([a-z]*\).*/\1/p' <<<"$copy_line")"
ok=0; le "$p_idle" 16 && le "$p_copy" 16 && [ "$running" = true ] && ok=1
check A-P-1 $ok "p99 key-to-flush idle $p_idle ms, during a 10 GiB copy to ext4 $p_copy ms (<= 16); job still running after the samples: $running"
fi

# ---- A-P-5 ---------------------------------------------------------------------------------
if want A-P-5; then
line="$(driver idle "$bin" "$src/k1a" 60)"
sb="" sa="" tb="" ta=""
read -r sb sa tb ta < <(sed -n 's/.*switches_before=\([0-9]*\) switches_after=\([0-9]*\) ticks_before=\([0-9]*\) ticks_after=\([0-9]*\).*/\1 \2 \3 \4/p' <<<"$line")
ok=0; [ -n "$sa" ] && [ -n "$ta" ] && [ "$sb" = "$sa" ] && [ "$tb" = "$ta" ] && ok=1
check A-P-5 $ok "60 s idle: voluntary context switches $sb -> $sa, CPU ticks $tb -> $ta (unchanged)"
fi

# ---- A-P-6 (and the RSS after Ctrl+R, below) ---------------------------------------------------
rss_refreshed=""
if want A-P-6 || want RSS/Ctrl+R; then
line="$(driver rss "$bin" "$src/many" "$src/many" 10)"
rss="$(field rss_mb "$line")"; rss_refreshed="$(field after_refreshes_mb "$line")"
if want A-P-6; then
ok=0; le "$rss" 40 && ok=1
check A-P-6 $ok "RSS $rss MB with both panels on 100k entries (<= 40)"
fi
fi

# ---- A-P-7 ---------------------------------------------------------------------------------
if want A-P-7; then
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
fi

# ---- A-P-8 ---------------------------------------------------------------------------------
if want A-P-8; then
mkdir -p "${MC_XDEV_DIR:-/dev/shm/mc-xdev}"
out="$(MC_REQUIRE_ALL=1 MC_XDEV_DIR="${MC_XDEV_DIR:-/dev/shm/mc-xdev}" cargo test --release --quiet --test fs_copy a_p_8_progress_is_capped_at_15_hz -- --exact 2>&1)" && ok=1 || ok=0
grep -q '1 passed' <<<"$out" || ok=0
check A-P-8 $ok "a 96 MiB copy tmpfs to btrfs: no two progress updates closer than 1/15 s (fs_copy a_p_8_progress_is_capped_at_15_hz, release: $(grep -oE '[0-9]+ passed; [0-9]+ failed' <<<"$out" | head -1))"
fi

# ---- phase 2: criterion (A-QF-3, A-CD-3, A-MR-7, A-DJ-5, P-1/Ctrl+R, P-1/refresh) ---------------
if want A-QF-3 || want A-CD-3 || want A-MR-7 || want A-DJ-5 || want P-1/Ctrl+R || want P-1/refresh; then
say "criterion: benches/phase2.rs"
cargo bench --quiet --bench phase2 -- --noplot >/dev/null 2>"$dir/phase2.err" || sed -n '1,40p' "$dir/phase2.err" >&2
fi

# ---- A-QF-3 (P-12) -------------------------------------------------------------------------
if want A-QF-3; then
f_sub="$(est p12/substring)"; f_first="$(est p12/first_char)"; f_glob="$(est p12/glob)"
line="$(driver filter "$bin" "$src" many 100)"
f_p99="$(field p99_ms "$line")"; f_max="$(field max_ms "$line")"
ok=0; le "$f_sub" 16 && le "$f_first" 16 && le "$f_glob" 16 && le "$f_p99" 16 && ok=1
check A-QF-3 $ok "$(printf 're-filter of 100k entries per keystroke: substring %.2f ms, first character (all match) %.2f ms, glob %.2f ms (<= 16); on a pty, Ctrl+F and 100 keystrokes: key-to-flush p99 %s ms, max %s ms (<= 16)' "$f_sub" "$f_first" "$f_glob" "$f_p99" "$f_max")"
fi

# ---- A-CD-3 (P-13) -------------------------------------------------------------------------
if want A-CD-3; then
c_thread="$(est p13/compare_thread)"; c_ui="$(est p13/ui_share)"
ok=0; le "$c_thread" 30 && le "$c_ui" 5 && ok=1
check A-CD-3 $ok "$(printf 'two 100k-entry listings by date and size: compare thread %.2f ms (<= 30), UI share (copy of both panels'"'"' visible entries) %.2f ms (<= 5)' "$c_thread" "$c_ui")"
fi

# ---- A-MR-7 (P-14) -------------------------------------------------------------------------
if want A-MR-7; then
r_mask="$(est p14/name_mask)"; r_date="$(est p14/counter_date_search)"; r_regex="$(est p14/regex_replace)"
ok=0; le "$r_mask" 16 && le "$r_date" 16 && le "$r_regex" 16 && ok=1
check A-MR-7 $ok "$(printf 'one keystroke with 10k names (edit, preview, checks): name mask %.2f ms, counter + date + search + title case %.2f ms, regex replace (compiled again) %.2f ms (<= 16)' "$r_mask" "$r_date" "$r_regex")"
fi

# ---- A-DJ-5 (P-15) -------------------------------------------------------------------------
if want A-DJ-5; then
tsv="$(fixture dirs-tsv)"
d_open="$(est p15/open)"; d_key="$(est p15/keystroke)"
line="$(driver dirs-dialog "$bin" "$src/k1a" "$tsv" 48)"
d_popen="$(field open_ms "$line")"; d_p99="$(field p99_ms "$line")"
line="$(driver first-frame "$bin" "$src/k1a" "$src/k1b" 20 "$tsv")"
ffs_med="$(sed -n 's/.*median=\([0-9.]*\).*/\1/p' <<<"$line")"; ffs_max="$(sed -n 's/.*max=\([0-9.]*\).*/\1/p' <<<"$line")"
ok=0; le "$d_open" 16 && le "$d_key" 16 && le "$d_popen" 16 && le "$d_p99" 16 && le "$ffs_med" 50 && ok=1
check A-DJ-5 $ok "$(printf '5000 frecency entries: rank and fill (Ctrl+D) %.2f ms, re-filter per keystroke %.3f ms (<= 16); on a pty, Ctrl+D %s ms, 48 keystrokes p99 %s ms (<= 16); first full frame with that dirs.tsv median %s ms, max %s ms over 20 starts (<= 50%s)' \
  "$d_open" "$d_key" "$d_popen" "$d_p99" "$ffs_med" "$ffs_max" "${ff_med:+; without it $ff_med ms}")"
fi

# ---- A-FD-5 (P-10) -------------------------------------------------------------------------
if want A-FD-5; then
tree="$(fixture tree)"
line="$(driver find "$tree" report - fold 20)"
n_all="$(field complete_ms "$line")"; n_first="$(field first_ms "$line")"; n_res="$(field results "$line")"
line="$(driver find "$tree" '' - fold 20)"
a_all="$(field complete_ms "$line")"; a_first="$(field first_ms "$line")"; a_res="$(field results "$line")"
read -r fd_s mc_s < <(hfn p10 "fd -uu -j $workers -F report $tree" "$drv find $tree report - fold 1")
read -r fda_s mca_s < <(hfn p10all "fd -uu -j $workers '' $tree" "$drv find $tree '' - fold 1")
r_n="$(ratio "$mc_s" "$fd_s")"; r_a="$(ratio "$mca_s" "$fda_s")"
ok=0; le "$n_all" 300 && le "$n_first" 50 && le "$r_n" 1.5 && ok=1
check A-FD-5 $ok "100k-entry tree, name \`report\` ($n_res results): complete $n_all ms (<= 300), first batch $n_first ms (<= 50); process $(ms "$mc_s") ms vs \`fd -uu -j $workers -F\` $(ms "$fd_s") ms (x$r_n, <= 1.5). Every name ($a_res results, each statx'ed for its columns): complete $a_all ms, first batch $a_first ms; process $(ms "$mca_s") ms vs \`fd -uu\` $(ms "$fda_s") ms (x$r_a)"
fi

# ---- A-FD-6 (P-11) -------------------------------------------------------------------------
if want A-FD-6; then
text="$(fixture text)"; needle="$(fixture needle)"
read -r rg_s mct_s < <(hfn p11 "rg -uuu -F -l -j $workers $needle $text" "$drv find $text '' $needle case 1")
read -r rgi_s mci_s < <(hfn p11i "rg -uuu -F -l -i -j $workers $needle $text" "$drv find $text '' $needle fold 1")
# After hyperfine's warm-up runs: after A-P-1 and A-P-7, one warm-up run of its own leaves
# part of the 1 GiB out of the page cache.
line="$(driver find "$text" '' "$needle" case 5)"
t_all="$(field complete_ms "$line")"; t_res="$(field results "$line")"
r_t="$(ratio "$mct_s" "$rg_s")"; r_i="$(ratio "$mci_s" "$rgi_s")"
ok=0; le "$r_t" 2 && ok=1
check A-FD-6 $ok "1 GiB of text in 10k files, a needle in $t_res of them: process $(ms "$mct_s") ms vs \`rg -uuu -F -l -j $workers\` $(ms "$rg_s") ms (x$r_t, <= 2); in-process $t_all ms. Case-folded: $(ms "$mci_s") ms vs \`rg -uuu -F -l -i\` $(ms "$rgi_s") ms (x$r_i)"
fi

# ---- A-SP-2 (P-16) -------------------------------------------------------------------------
if want A-SP-2; then
sp="$(fixture sparse)"
avail="$(df -B1 --output=avail /dev/shm | tail -1 | tr -d ' ')"
if [ "$avail" -lt $((64 << 20)) ]; then
  check A-SP-2 0 "not run: /dev/shm has $avail bytes free, 64 MiB needed"
else
  mkdir -p "$shm"
  times=()
  for _ in 1 2 3 4 5; do
    rm -f "$shm/$(basename "$sp")"
    times+=("$(field engine_s "$(driver copy "$(dirname "$sp")" "$(basename "$sp")" "$shm")")")
  done
  sp_s="$(printf '%s\n' "${times[@]}" | sort -g | sed -n 3p)"
  alloc() { stat -c '%b %B' "$1" | awk '{ print $1 * $2 }'; }
  sa="$(alloc "$sp")"; da="$(alloc "$shm/$(basename "$sp")" 2>/dev/null)" || da=missing
  same=no; [ "$(stat -c %s "$sp")" = "$(stat -c %s "$shm/$(basename "$sp")" 2>/dev/null)" ] && same=yes
  rm -rf "$shm"
  ok=0; le "$sp_s" 1 && le "$da" "$((sa + (1 << 20)))" && [ "$same" = yes ] && ok=1
  check A-SP-2 $ok "$(printf '16 GiB file with 8 MiB of data, btrfs to tmpfs: %.3f s median of 5 (<= 1); allocation %s bytes at the source, %s at the destination (<= source + 1 MiB); same size: %s' "$sp_s" "$sa" "$da" "$same")"
fi
fi

# ---- A-HL-4 (P-17) -------------------------------------------------------------------------
if want A-HL-4; then
read -r hl _ < <(fixture hardlinks)
pd="$(dirname "$hl")"
read -r t_hl t_nohl < <(hfp p17 "rm -rf $mnt/hl $mnt/nohl; sync" 5 "$drv copy $pd hl $mnt" "$drv copy $pd nohl $mnt")
clean_ext4
driver copy "$pd" hl "$mnt" >/dev/null
names="$(find "$mnt/hl" -type f 2>/dev/null | wc -l)" || true
linked="$(find "$mnt/hl" -type f -links 2 2>/dev/null | wc -l)" || true
inodes="$(find "$mnt/hl" -type f -links 2 -printf '%i\n' 2>/dev/null | sort -u | wc -l)" || true
clean_ext4
r_hl="$(ratio "$t_hl" "$t_nohl")"
ok=0; le "$r_hl" 1 && [ "$linked" = 20000 ] && [ "$inodes" = 10000 ] && ok=1
check A-HL-4 $ok "$(printf '10k hard-link pairs (20k names of 4 KiB) btrfs to ext4: %.2f s vs the same 20k names as separate files %.2f s (x%s, <= 1), median of 5; destination: %s names, %s with 2 links on %s inodes' "$t_hl" "$t_nohl" "$r_hl" "$names" "$linked" "$inodes")"
fi

# ---- P-1/refresh ---------------------------------------------------------------------------
if want P-1/refresh; then
rs_apply="$(est restat/apply)"; rd_apply="$(est restat/dir_apply)"
ok=0; le "$rs_apply" 16 && le "$rd_apply" 16 && ok=1
check P-1/refresh $ok "$(printf 'UI thread when a refresh of 100k entries completes (swapped in, collation keys built, sorted), in-process: a results tab'"'"'s re-stat %.2f ms, a directory'"'"'s re-listing (M1) %.2f ms (<= 16: a key that arrives meanwhile waits)' "$rs_apply" "$rd_apply")"
fi

# ---- P-6b, P-1/Ctrl+R, RSS/Ctrl+R ------------------------------------------------------------
if want P-6b || want P-1/Ctrl+R || want RSS/Ctrl+R; then
tree="$(fixture tree)"
line="$(driver rss-results "$bin" "$tree" "$src/many" 100000 10)"
rr_res="$(field results_mb "$line")"; rr_dirs="$(field dirs_mb "$line")"
rr_rst="$(field restats_mb "$line")"; rr_after="$(field dirs_after_mb "$line")"
rs_p50="$(field restat_p50_ms "$line")"; rs_max="$(field max_ms "$line")"
if want P-6b; then
ok=0; le "$rr_dirs" 60 && le "$rr_res" 60 && ok=1
check P-6b $ok "RSS $rr_dirs MB with both panels on 100k entries and a hidden tab of 100k results (the find of every name of the 100k-entry tree); $rr_res MB with the results tab on screen (<= 60)"
fi
if want P-1/Ctrl+R; then
rs_copy="$(est restat/copy)"
ok=0; le "$rs_max" 16 && le "$rs_copy" 16 && ok=1
check P-1/Ctrl+R $ok "$(printf 'Ctrl+R in a tab of 100k results beside a 100k-entry panel: key-to-flush p50 %s ms, max %s ms over 10 (<= 16); the UI thread'"'"'s copy of the results for the re-stat, in-process, %.2f ms' "$rs_p50" "$rs_max" "$rs_copy")"
fi
if want RSS/Ctrl+R; then
ok=0; le "$rss_refreshed" 40 && le "$rr_after" 60 && le "$rr_rst" 60 && ok=1
check RSS/Ctrl+R $ok "after 10 Ctrl+R, 1.5 s apart: both panels on 100k entries $rss_refreshed MB (A-P-6 limit 40); the 100k-result tab on screen $rr_rst MB, then both panels on 100k entries with it hidden $rr_after MB (P-6b limit 60)"
fi
fi

# ---- history -------------------------------------------------------------------------------
if [ "${MC_BENCH_HISTORY:-1}" != 0 ]; then
{
  printf '\n## %s\n\nConditions: %s. Commit %s%s.\n\n| Check | Result | Measurement |\n|---|---|---|\n' \
    "$(date '+%Y-%m-%d %H:%M')" "$cond" "$(git rev-parse --short HEAD)" "$(git diff --quiet HEAD -- src || echo ' (uncommitted changes in src)')"
  for id in A-P-1 A-P-2 A-P-3 A-P-4 A-P-5 A-P-6 A-P-7 A-P-8 A-QF-3 A-CD-3 A-MR-7 A-DJ-5 \
    A-FD-5 A-FD-6 A-SP-2 A-HL-4 P-6b P-1/Ctrl+R P-1/refresh RSS/Ctrl+R; do
    [ -n "${result[$id]:-}" ] || continue
    printf '| %s | %s | %s |\n' "$id" "${result[$id]}" "${note[$id]}"
  done
} >> "$hist"
say "appended to $hist"
fi
[ "$fails" -eq 0 ]
