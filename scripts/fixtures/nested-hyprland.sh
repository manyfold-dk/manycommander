#!/usr/bin/env bash
# nested-hyprland.sh COMMAND [ARGS...] -- run COMMAND with a nested Hyprland to start real
# terminals in (A-QV-8 in tests/manual.rs). The running Hyprland session starts the nested
# one on a hidden special workspace (nothing takes the keyboard focus; render_unfocused keeps
# it drawing for grim). COMMAND gets the nested session in WAYLAND_DISPLAY and
# HYPRLAND_INSTANCE_SIGNATURE, and MC_NESTED=1. MC_NESTED_LAYOUT sets the keyboard layout
# (default us). On exit, the trap stops the nested Hyprland by its pid.
set -euo pipefail

[ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] || { echo "nested-hyprland: needs a running Hyprland session" >&2; exit 1; }
run="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
work="$(mktemp -d "${TMPDIR:-/tmp}/mc-nested.XXXXXX")"
cfg="$work/hyprland.lua"
pid=""
cleanup() {
  [ -n "$pid" ] && kill "$pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT INT TERM

cat > "$cfg" <<EOF
hl.monitor({ output = "", mode = "1280x800", position = "0x0", scale = 1 })
hl.config({
  input = { kb_layout = "${MC_NESTED_LAYOUT:-us}" },
  animations = { enabled = false },
  general = { gaps_in = 0, gaps_out = 0, border_size = 0 },
  decoration = { rounding = 0 },
  misc = { disable_hyprland_logo = true, disable_splash_rendering = true },
})
EOF

sigs_before="$(ls "$run/hypr")"
socks_before="$(ls "$run" | grep -E '^wayland-[0-9]+$' || true)"
out="$(hyprctl dispatch "hl.dsp.exec_cmd(\"Hyprland -c $cfg\", { workspace = \"special:mc-nested silent\", render_unfocused = true })")"
[ "$out" = ok ] || { echo "nested-hyprland: $out" >&2; exit 1; }
sig="" sock=""
for _ in $(seq 100); do
  sig="$(comm -13 <(echo "$sigs_before") <(ls "$run/hypr") | head -1)"
  sock="$(comm -13 <(echo "$socks_before") <(ls "$run" | grep -E '^wayland-[0-9]+$' || true) | head -1)"
  [ -n "$sig" ] && [ -n "$sock" ] && break
  sleep 0.1
done
[ -n "$sig" ] && [ -n "$sock" ] || { echo "nested-hyprland: the nested session did not appear" >&2; exit 1; }
pid="$(pgrep -n -f "Hyprland -c $cfg")"
echo "nested-hyprland: $sock ($sig), pid $pid" >&2
env -u DISPLAY -u TMUX -u TMUX_PANE WAYLAND_DISPLAY="$sock" HYPRLAND_INSTANCE_SIGNATURE="$sig" MC_NESTED=1 "$@"
