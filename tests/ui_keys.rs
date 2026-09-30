//! T11: every chord of the design section 8 table, and of P2 10 and P3 6 (A-KM-1),
//! reaches manycommander as the intended action, in the encodings the Omarchy terminals
//! send: legacy xterm, and the kitty keyboard protocol with `DISAMBIGUATE_ESCAPE_CODES`.
//! Which chords the terminals keep for themselves is the configuration audit in the plans.

mod common;

use common::tui::*;
use common::*;
use std::time::Duration;

const T: Duration = Duration::from_secs(10);

/// `(chord, legacy bytes, kitty-protocol bytes, action without the focus on the command
/// line)`.
const CHORDS: &[(&str, &[u8], &[u8], &str)] = &[
    ("Tab", b"\t", b"\t", "SwitchPanel"),
    ("Up", b"\x1b[A", b"\x1b[A", "Up"),
    ("Down", b"\x1b[B", b"\x1b[B", "Down"),
    ("PgUp", b"\x1b[5~", b"\x1b[5~", "PageUp"),
    ("PgDn", b"\x1b[6~", b"\x1b[6~", "PageDown"),
    ("Home", b"\x1b[H", b"\x1b[H", "First"),
    ("End", b"\x1b[F", b"\x1b[F", "Last"),
    ("Insert", b"\x1b[2~", b"\x1b[2~", "MarkAndDown"),
    ("Space", b" ", b" ", "MarkSpace"),
    ("Backspace", b"\x7f", b"\x7f", "Parent"),
    ("Ctrl+H", b"\x08", b"\x1b[104;5u", "Parent"),
    ("Alt+Up", b"\x1b[1;3A", b"\x1b[1;3A", "Parent"),
    ("Alt+Left", b"\x1b[1;3D", b"\x1b[1;3D", "HistoryBack"),
    ("Alt+Right", b"\x1b[1;3C", b"\x1b[1;3C", "HistoryForward"),
    ("Ctrl+A", b"\x01", b"\x1b[97;5u", "MarkAll"),
    ("Ctrl+U", b"\x15", b"\x1b[117;5u", "SwapPanels"),
    ("Ctrl+U", b"\x15", b"\x1b[117;5u", "SwapPanels"),
    ("Ctrl+P", b"\x10", b"\x1b[112;5u", "HistoryPrev"),
    ("Ctrl+N", b"\x0e", b"\x1b[110;5u", "HistoryNext"),
    ("Alt+.", b"\x1b.", b"\x1b[46;3u", "ToggleHidden"),
    ("Alt+.", b"\x1b.", b"\x1b[46;3u", "ToggleHidden"),
    ("Ctrl+R", b"\x12", b"\x1b[114;5u", "Reread"),
    ("Alt+*", b"\x1b*", b"\x1b[42;3u", "InvertMarks"),
    ("Alt+*", b"\x1b*", b"\x1b[42;3u", "InvertMarks"),
    ("Ctrl+F4", b"\x1b[1;5S", b"\x1b[1;5S", "Sort(Ext)"),
    ("Ctrl+F5", b"\x1b[15;5~", b"\x1b[15;5~", "Sort(Size)"),
    ("Ctrl+F6", b"\x1b[17;5~", b"\x1b[17;5~", "Sort(Mtime)"),
    ("Ctrl+T", b"\x14", b"\x1b[116;5u", "NewTab"),
    ("Alt+PgUp", b"\x1b[5;3~", b"\x1b[5;3~", "PrevTab"),
    ("Alt+PgDn", b"\x1b[6;3~", b"\x1b[6;3~", "NextTab"),
    ("Ctrl+W", b"\x17", b"\x1b[119;5u", "CloseTab"),
    ("F3", b"\x1bOR", b"\x1bOR", "View"),
    ("F4", b"\x1bOS", b"\x1bOS", "Edit"),
    ("F5", b"\x1b[15~", b"\x1b[15~", "Copy"),
    ("F6", b"\x1b[17~", b"\x1b[17~", "Move"),
    ("F8", b"\x1b[19~", b"\x1b[19~", "Trash"),
    ("Shift+F8", b"\x1b[19;2~", b"\x1b[19;2~", "Delete"),
    ("Alt+Enter", b"\x1b\r", b"\x1b[13;3u", "InsertName"),
    ("Alt+P", b"\x1bp", b"\x1b[112;3u", "InsertPath"),
    ("Ctrl+S", b"\x13", b"\x1b[115;5u", "QuickSearch"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    // P3 6: legacy Ctrl+Q is XON, which reaches the application because raw mode clears
    // IXON. The view on, Tab swaps sides twice, Alt+Q previews (nothing on `..`), the view
    // off; Alt+O on `..` opens nothing.
    ("Ctrl+Q", b"\x11", b"\x1b[113;5u", "QuickView"),
    ("Tab", b"\t", b"\t", "SwitchPanel"),
    ("Tab", b"\t", b"\t", "SwitchPanel"),
    ("Alt+Q", b"\x1bq", b"\x1b[113;3u", "QuickLoad"),
    ("Ctrl+Q", b"\x11", b"\x1b[113;5u", "QuickView"),
    ("Alt+O", b"\x1bo", b"\x1b[111;3u", "OpenArchive"),
    // Dialogs from here on: the key still reaches the runtime and is logged.
    ("F1", b"\x1bOP", b"\x1bOP", "Help"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Alt+=", b"\x1b=", b"\x1b[61;3u", "MarkGlob"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Alt+-", b"\x1b-", b"\x1b[45;3u", "UnmarkGlob"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("F7", b"\x1b[18~", b"\x1b[18~", "Mkdir"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Shift+F4", b"\x1b[1;2S", b"\x1b[1;2S", "EditNew"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Shift+F6", b"\x1b[17;2~", b"\x1b[17;2~", "Rename"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    // P2 10 (the panel is empty, so no form opens; the chord is still read and mapped).
    ("Alt+L", b"\x1bl", b"\x1b[108;3u", "Link"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Alt+A", b"\x1ba", b"\x1b[97;3u", "Attributes"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    // The filter line takes Esc (it clears the filter); the compare form opens on two
    // directory panels.
    ("Ctrl+F", b"\x06", b"\x1b[102;5u", "Filter"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    // Typing opens the filter line; Ctrl+E gives the command line the focus, Esc takes
    // it back.
    ("a", b"a", b"a", "FilterChar('a')"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Ctrl+E", b"\x05", b"\x1b[101;5u", "FocusLine"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    ("Shift+F2", b"\x1b[1;2Q", b"\x1b[1;2Q", "Compare"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
    // P2 5.1: the find form.
    ("Alt+F7", b"\x1b[18;3~", b"\x1b[18;3~", "Find"),
    ("Esc", b"\x1b", b"\x1b[27u", "Escape"),
];

/// Chords whose protocol encoding exists only with the protocol.
const KITTY_ONLY: &[(&str, &[u8], &str)] = &[
    // Legacy Ctrl+F3 is `CSI 1;5 R`, the cursor position report; the protocol sends F3 as
    // `CSI 13 ~`.
    ("Ctrl+F3", b"\x1b[13;5~", "Sort(Name)"),
    ("Ctrl+1", b"\x1b[49;5u", "GotoTab(1)"),
    ("Ctrl+2", b"\x1b[50;5u", "GotoTab(2)"),
    ("Ctrl+9", b"\x1b[57;5u", "GotoTab(9)"),
];

fn run(kitty: bool) {
    let h = test_dir(if kitty {
        "ui-keys-kitty"
    } else {
        "ui-keys-legacy"
    });
    std::fs::create_dir_all(h.join("empty")).unwrap();
    let log = h.join("keys.log");
    let empty = h.join("empty");
    let mut t = Tui::spawn_opts(
        &[
            "--log",
            log.to_str().unwrap(),
            empty.to_str().unwrap(),
            empty.to_str().unwrap(),
        ],
        &h.path,
        &[],
        120,
        30,
        kitty,
    );
    assert!(t.wait_for("10Quit", T), "{}", t.screen());
    let mut expect: Vec<(&str, &str)> = Vec::new();
    for (name, legacy, proto, action) in CHORDS {
        t.keys(&[if kitty { proto } else { legacy }]);
        std::thread::sleep(Duration::from_millis(40));
        expect.push((name, action));
    }
    if kitty {
        for (name, seq, action) in KITTY_ONLY {
            t.keys(&[seq]);
            expect.push((name, action));
        }
    }
    t.keys(&[F10]);
    // A job-free F10 quits at once.
    assert_eq!(t.wait_exit(T), Some(0), "{}", t.screen());
    let text = std::fs::read_to_string(&log).unwrap();
    let got: Vec<String> = text
        .lines()
        .filter(|l| l.contains(" key ") && l.contains("action="))
        .map(|l| {
            l.split("action=")
                .nth(1)
                .unwrap()
                .split_whitespace()
                .next()
                .unwrap()
                .to_string()
        })
        .collect();
    let got = &got[..got.len() - 1]; // the final F10
    let want: Vec<&str> = expect.iter().map(|(_, a)| *a).collect();
    for (i, (w, g)) in want.iter().zip(got.iter()).enumerate() {
        assert_eq!(w, g, "chord #{i} {} (kitty={kitty})", expect[i].0);
    }
    assert_eq!(
        got.len(),
        want.len(),
        "every chord was read once (kitty={kitty}): {got:?}"
    );
}

#[test]
fn chords_in_legacy_encoding() {
    run(false);
}

#[test]
fn chords_with_the_kitty_keyboard_protocol() {
    run(true);
}
