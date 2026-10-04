#!/usr/bin/env python3
"""record-keys.py [--terminal NAME]... [--layout NAME]... -- record the bytes that real
terminals send for manycommander's chords, on real keyboard layouts.

`tests/ui_keys.rs` replays the recordings (tests/fixtures/keys/TERMINAL-LAYOUT.tsv): the
bytes a chord produces depend on the terminal, the keyboard layout and the keyboard protocol
flags, and hand-written encodings miss that (`Alt+*` on a Spanish layout arrives as `+` with
`Shift`, plus the shifted and the base-layout key).

Run it on a Hyprland desktop. For each layout it starts a nested Hyprland with that layout on
a hidden special workspace of the running session (nothing takes the keyboard focus), starts
the terminal inside it with a capture program that pushes the kitty keyboard flags
manycommander pushes (5) and logs every read, and presses each chord with Hyprland's
`send_shortcut`, which sends the physical key through the layout's keymap. A chord that the
terminal keeps for itself records `-`. Everything it starts is stopped by pid on exit.

Needs: Hyprland (Lua configuration), ghostty and/or foot, python3. Default: both terminals,
layouts us and es.
"""
import argparse
import os
import select
import signal
import subprocess
import sys
import termios
import time
import tty
from pathlib import Path

FLAGS = 5  # DISAMBIGUATE_ESCAPE_CODES | REPORT_ALTERNATE_KEYS, as src/app/term.rs pushes

# The chords of tests/ui_keys.rs: name -> (Hyprland modifiers, key on the layout's first
# level). A layout overrides the chords whose character it types elsewhere.
CHORDS = {
    "Tab": ("", "Tab"),
    "Up": ("", "Up"),
    "Down": ("", "Down"),
    "PgUp": ("", "Prior"),
    "PgDn": ("", "Next"),
    "Home": ("", "Home"),
    "End": ("", "End"),
    "Insert": ("", "Insert"),
    "Space": ("", "space"),
    "Backspace": ("", "BackSpace"),
    "Ctrl+H": ("CTRL", "h"),
    "Alt+Up": ("ALT", "Up"),
    "Alt+Left": ("ALT", "Left"),
    "Alt+Right": ("ALT", "Right"),
    "Ctrl+A": ("CTRL", "a"),
    "Ctrl+U": ("CTRL", "u"),
    "Ctrl+P": ("CTRL", "p"),
    "Ctrl+N": ("CTRL", "n"),
    "Alt+.": ("ALT", "period"),
    "Ctrl+R": ("CTRL", "r"),
    "Alt+*": ("ALT SHIFT", "8"),
    "Ctrl+F3": ("CTRL", "F3"),
    "Ctrl+F4": ("CTRL", "F4"),
    "Ctrl+F5": ("CTRL", "F5"),
    "Ctrl+F6": ("CTRL", "F6"),
    "Ctrl+T": ("CTRL", "t"),
    "Alt+PgUp": ("ALT", "Prior"),
    "Alt+PgDn": ("ALT", "Next"),
    "Ctrl+W": ("CTRL", "w"),
    "F1": ("", "F1"),
    "F3": ("", "F3"),
    "F4": ("", "F4"),
    "F5": ("", "F5"),
    "F6": ("", "F6"),
    "F7": ("", "F7"),
    "F8": ("", "F8"),
    "Shift+F2": ("SHIFT", "F2"),
    "Shift+F4": ("SHIFT", "F4"),
    "Shift+F6": ("SHIFT", "F6"),
    "Shift+F8": ("SHIFT", "F8"),
    "Alt+F7": ("ALT", "F7"),
    "Alt+Enter": ("ALT", "Return"),
    "Alt+P": ("ALT", "p"),
    "Alt+Q": ("ALT", "q"),
    "Alt+O": ("ALT", "o"),
    "Alt+L": ("ALT", "l"),
    "Alt+A": ("ALT", "a"),
    "Alt+=": ("ALT", "equal"),
    "Alt+-": ("ALT", "minus"),
    "Ctrl+S": ("CTRL", "s"),
    "Ctrl+Q": ("CTRL", "q"),
    "Ctrl+F": ("CTRL", "f"),
    "Ctrl+E": ("CTRL", "e"),
    "Ctrl+1": ("CTRL", "1"),
    "Ctrl+2": ("CTRL", "2"),
    "Ctrl+9": ("CTRL", "9"),
    "Esc": ("", "Escape"),
    "a": ("", "a"),
}
LAYOUTS = {
    "us": {},
    # Spanish: `*` is Shift plus the `+` key, `=` is Shift plus `0`.
    "es": {"Alt+*": ("ALT SHIFT", "plus"), "Alt+=": ("ALT SHIFT", "0")},
}
TERMINALS = ("ghostty", "foot")


def capture(out: str) -> None:
    """Inside the terminal: push the flags, log every read as a hex line, pop on exit."""
    log = open(out, "a", buffering=1)
    fd = sys.stdin.fileno()
    old = termios.tcgetattr(fd)
    tty.setraw(fd)
    os.write(1, f"\x1b[>{FLAGS}u".encode())
    log.write("READY\n")
    try:
        while True:
            r, _, _ = select.select([fd], [], [], 120)
            if not r:
                break
            log.write(os.read(fd, 1024).hex() + "\n")
    finally:
        os.write(1, b"\x1b[<u")
        termios.tcsetattr(fd, termios.TCSADRAIN, old)


def hypr(lua: str, nested: tuple[str, str] | None = None) -> str:
    """`hyprctl dispatch` on the running session, or on the nested one (signature, display)."""
    env = dict(os.environ)
    if nested:
        env["HYPRLAND_INSTANCE_SIGNATURE"], env["WAYLAND_DISPLAY"] = nested
    r = subprocess.run(["hyprctl", "dispatch", lua], env=env, capture_output=True, text=True)
    return (r.stdout + r.stderr).strip()


def runtime() -> str:
    return os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}")


def start_nested(layout: str, work: Path) -> tuple[int, str, str]:
    """A nested Hyprland with `layout`; returns its pid, instance signature and display."""
    cfg = work / f"nested-{layout}.lua"
    cfg.write_text(
        "hl.monitor({ output = \"\", mode = \"1280x800\", position = \"0x0\", scale = 1 })\n"
        "hl.config({\n"
        f"  input = {{ kb_layout = \"{layout}\" }},\n"
        "  animations = { enabled = false },\n"
        "  misc = { disable_hyprland_logo = true, disable_splash_rendering = true },\n"
        "})\n"
    )
    hypr_dir = Path(runtime()) / "hypr"
    before = set(os.listdir(hypr_dir))
    sockets = set(p for p in os.listdir(runtime()) if p.startswith("wayland-"))
    r = hypr(
        f'hl.dsp.exec_cmd("Hyprland -c {cfg}", '
        '{ workspace = "special:mc-record-keys silent", render_unfocused = true })'
    )
    if r != "ok":
        sys.exit(f"record-keys: starting the nested Hyprland failed: {r}")
    for _ in range(100):
        new = set(os.listdir(hypr_dir)) - before
        disp = set(p for p in os.listdir(runtime()) if p.startswith("wayland-")) - sockets
        disp = {d for d in disp if not d.endswith(".lock")}
        if new and disp:
            break
        time.sleep(0.1)
    else:
        sys.exit("record-keys: the nested Hyprland did not appear")
    sig = new.pop()
    display = disp.pop()
    pid = int(subprocess.check_output(["pgrep", "-n", "-f", f"Hyprland -c {cfg}"]).split()[0])
    return pid, sig, display


def record(terminal: str, layout: str, work: Path, out_dir: Path) -> None:
    pid, sig, display = start_nested(layout, work)
    cap = work / f"cap-{terminal}-{layout}.txt"
    cap.unlink(missing_ok=True)
    env = {k: v for k, v in os.environ.items() if not k.startswith(("TMUX", "TERM_PROGRAM"))}
    env.update(WAYLAND_DISPLAY=display, HYPRLAND_INSTANCE_SIGNATURE=sig)
    env.pop("DISPLAY", None)
    me = str(Path(__file__).resolve())
    if terminal == "ghostty":
        argv = ["ghostty", "--gtk-single-instance=false", "-e", sys.executable, me, "--capture", str(cap)]
    else:
        argv = ["foot", sys.executable, me, "--capture", str(cap)]
    term = subprocess.Popen(argv, env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                            start_new_session=True)
    rows = []
    try:
        for _ in range(100):
            if cap.exists() and "READY" in cap.read_text():
                break
            time.sleep(0.1)
        else:
            sys.exit(f"record-keys: {terminal} did not start the capture")
        time.sleep(0.5)
        chords = dict(CHORDS, **LAYOUTS[layout])
        for name, (mods, key) in chords.items():
            seen = len(cap.read_text().splitlines())
            r = hypr(f'hl.dsp.send_shortcut({{ mods = "{mods}", key = "{key}" }})', (sig, display))
            if r != "ok":
                sys.exit(f"record-keys: {name}: send_shortcut: {r}")
            time.sleep(0.3)
            if term.poll() is not None:
                sys.exit(f"record-keys: {terminal} ended after {name}")
            got = "".join(cap.read_text().splitlines()[seen:])
            rows.append((name, mods, key, got or "-"))
    finally:
        os.killpg(term.pid, signal.SIGTERM)
        os.kill(pid, signal.SIGTERM)
        time.sleep(1)
    out = out_dir / f"{terminal}-{layout}.tsv"
    with open(out, "w") as f:
        f.write(f"# {terminal}, keyboard layout {layout}, kitty keyboard flags {FLAGS}; "
                "recorded by scripts/fixtures/record-keys.py.\n")
        f.write("# chord\tmodifiers sent\tkey sent\tbytes (hex; - when the terminal kept the chord)\n")
        for name, mods, key, got in rows:
            f.write(f"{name}\t{mods or '-'}\t{key}\t{got}\n")
    print(f"record-keys: {out}: {sum(1 for r in rows if r[3] != '-')} of {len(rows)} chords sent bytes")


def main() -> None:
    if len(sys.argv) == 3 and sys.argv[1] == "--capture":
        capture(sys.argv[2])
        return
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--terminal", action="append", choices=TERMINALS)
    ap.add_argument("--layout", action="append", choices=sorted(LAYOUTS))
    a = ap.parse_args()
    top = Path(__file__).resolve().parents[2]
    out_dir = top / "tests/fixtures/keys"
    out_dir.mkdir(parents=True, exist_ok=True)
    work = Path(os.environ.get("TMPDIR", "/tmp")) / f"mc-record-keys-{os.getpid()}"
    work.mkdir()
    for layout in a.layout or ["us", "es"]:
        for terminal in a.terminal or list(TERMINALS):
            record(terminal, layout, work, out_dir)
    for p in work.iterdir():
        p.unlink()
    work.rmdir()


if __name__ == "__main__":
    main()
