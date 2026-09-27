#!/usr/bin/env bash
# stall-fuse.sh COMMAND [ARGS...] -- run COMMAND while an unresponsive FUSE mount exists
# (A-UI-1). An empty directory is mounted with `rclone mount --daemon` at a mount point
# under /tmp, then the rclone process is stopped with SIGSTOP: every access to the mount
# blocks. COMMAND gets the mount point in MC_STALL_MNT and its parent in MC_STALL_DIR.
# On exit, the trap resumes rclone and unmounts with fusermount3 -u.
set -euo pipefail

base="$(mktemp -d /tmp/mc-stall.XXXXXX)"
src="$base/src" mnt="$base/stuck"
mkdir -p "$src" "$mnt" "$base/fine"
touch "$base/fine/ok"
pid=""
cleanup() {
  [ -n "$pid" ] && kill -CONT "$pid" 2>/dev/null || true
  fusermount3 -u "$mnt" 2>/dev/null || fusermount3 -uz "$mnt" 2>/dev/null || true
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  rm -rf "$base" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

rclone mount "$src" "$mnt" --daemon --daemon-wait 10s --log-level ERROR
for _ in $(seq 50); do
  mountpoint -q "$mnt" && break
  sleep 0.1
done
mountpoint -q "$mnt" || { echo "stall-fuse: the rclone mount did not appear" >&2; exit 1; }
pid="$(pgrep -f "rclone mount $src $mnt" | head -1)"
[ -n "$pid" ] || { echo "stall-fuse: rclone process not found" >&2; exit 1; }
kill -STOP "$pid"
echo "stall-fuse: $mnt is stuck (rclone $pid stopped)" >&2
MC_STALL_MNT="$mnt" MC_STALL_DIR="$base" "$@"
