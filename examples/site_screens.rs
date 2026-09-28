#![forbid(unsafe_code)]
//! Renders the real UI to SVG screenshots for the project website.
//!
//! `cargo run --example site_screens -- [THEMES_DIR] [OUT_DIR]` writes `<name>.svg` for
//! every `<THEMES_DIR>/<name>/colors.toml`, `dialog.svg` in the default theme and
//! `themes.json` for the theme picker. Panels hold synthetic listings with fixed times, so
//! the output is byte-identical across runs.

use manycommander::app::App;
use manycommander::app::event::Event;
use manycommander::config::Config;
use manycommander::fsops::sys::{FsIdentity, Kind, Meta, Ts};
use manycommander::panel::Panel;
use manycommander::panel::entry::Entry;
use manycommander::panel::listing::{self, ListingMsg};
use manycommander::theme::palette::Rgb;
use manycommander::theme::{Depth, Palette};
use ratatui::Terminal;
use ratatui::backend::{Backend, TestBackend};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;
use unicode_width::UnicodeWidthStr;

/// The narrowest width with the extension column and full dates.
const COLS: u16 = 124;
const ROWS: u16 = 24;
/// Cell size in SVG units; integers keep every coordinate integral.
const CW: i32 = 10;
const CH: i32 = 20;
const FONT_SIZE: i32 = 16;
/// Baseline offset inside a cell.
const BASELINE: i32 = 15;
const DEFAULT_THEME: &str = "tokyo-night";
const FONTS: &str =
    r#""JetBrainsMono Nerd Font","JetBrains Mono",ui-monospace,"SF Mono",Menlo,Consolas,monospace"#;

/// 2026-09-01 00:00 UTC.
const SEP_1: i64 = 1_788_220_800;

fn at(day: i64, hour: i64, min: i64) -> i64 {
    SEP_1 + (day - 1) * 86_400 + hour * 3600 + min * 60
}

type Row = (&'static [u8], Kind, u32, u64, i64);

fn project() -> Vec<Row> {
    vec![
        (b".git", Kind::Dir, 0o755, 0, at(27, 18, 42)),
        (b"benches", Kind::Dir, 0o755, 0, at(19, 11, 5)),
        (b"docs", Kind::Dir, 0o755, 0, at(26, 16, 30)),
        (b"src", Kind::Dir, 0o755, 0, at(27, 18, 40)),
        (b"tests", Kind::Dir, 0o755, 0, at(25, 9, 12)),
        (b"Cargo.lock", Kind::File, 0o644, 41_873, at(24, 14, 3)),
        (b"Cargo.toml", Kind::File, 0o644, 1_412, at(24, 14, 3)),
        (b"LICENSE", Kind::File, 0o644, 11_357, at(2, 10, 0)),
        (b"README.md", Kind::File, 0o644, 6_218, at(26, 17, 48)),
        (b"rust-toolchain.toml", Kind::File, 0o644, 96, at(12, 8, 21)),
    ]
}

fn downloads() -> Vec<Row> {
    vec![
        (b"wallpapers", Kind::Dir, 0o755, 0, at(20, 21, 14)),
        (b".wget-hsts", Kind::File, 0o600, 165, at(9, 13, 37)),
        (
            b"archlinux-x86_64.iso",
            Kind::File,
            0o644,
            1_236_860_928,
            at(22, 19, 2),
        ),
        (
            b"backup-photos.tar.zst",
            Kind::File,
            0o644,
            8_914_223_104,
            at(15, 23, 50),
        ),
        (
            b"boarding-pass.pdf",
            Kind::File,
            0o644,
            214_530,
            at(26, 7, 45),
        ),
        (
            b"invoice-0917.pdf",
            Kind::File,
            0o644,
            88_412,
            at(17, 10, 26),
        ),
        (
            b"mountains.jpg",
            Kind::File,
            0o644,
            5_872_014,
            at(21, 18, 3),
        ),
        (b"rustup-init.sh", Kind::File, 0o755, 25_163, at(11, 12, 9)),
        (
            b"screenshot-01.png",
            Kind::File,
            0o644,
            734_208,
            at(27, 15, 31),
        ),
        (
            b"screenshot-02.png",
            Kind::File,
            0o644,
            698_880,
            at(27, 15, 33),
        ),
        (
            b"talk-slides.pdf",
            Kind::File,
            0o644,
            3_140_977,
            at(23, 20, 11),
        ),
    ]
}

fn meta(kind: Kind, perm: u32, size: u64, mtime: i64) -> Meta {
    let ts = Ts {
        sec: mtime,
        nsec: 0,
    };
    Meta {
        kind,
        perm,
        uid: 1000,
        nlink: 1,
        size,
        blocks: size.div_ceil(512),
        id: FsIdentity::default(),
        atime: ts,
        mtime: ts,
        ctime: ts,
        mount_root: false,
    }
}

/// Feeds a finished listing to the active tab of `side`, as a listing thread would.
fn fill(a: &mut App, side: usize, rows: &[Row], free: u64, total: u64) {
    let slot = a.sides[side].panel().slot;
    let dir = a.sides[side].panel().dir.clone();
    let req = a.sides[side]
        .panel_mut()
        .navigate(dir.clone(), None, listing::Alive::running());
    let mut names = Vec::new();
    let entries = rows
        .iter()
        .map(|(n, k, p, s, t)| Entry::new(&mut names, n, &meta(*k, *p, *s, *t)))
        .collect();
    let generation = req.generation;
    a.update(Event::Listing(ListingMsg::Batch {
        slot,
        generation,
        entries,
        names,
    }));
    a.update(Event::Listing(ListingMsg::Done {
        slot,
        generation,
        dir,
        elapsed: Duration::ZERO,
    }));
    a.update(Event::Listing(ListingMsg::FreeSpace {
        slot,
        generation,
        free,
        total,
    }));
}

fn scene(palette: Palette) -> App {
    let mut a = App::new(
        PathBuf::from("/home/you/code/manycommander"),
        PathBuf::from("/home/you/Downloads"),
        PathBuf::from("/home/you"),
        Config::default(),
        Some(palette),
        Depth::TrueColor,
        jiff::tz::TimeZone::UTC,
    );
    for dir in ["/home/you/Pictures", "/home/you/Music"] {
        let slot = a.new_slot();
        a.sides[1].tabs.push(Panel::new(slot, PathBuf::from(dir)));
    }
    let (free, total) = (312 << 30, 931 << 30);
    fill(&mut a, 0, &project(), free, total);
    fill(&mut a, 1, &downloads(), free, total);
    a.sides[0].panel_mut().ensure_sorted();
    a.sides[0].panel_mut().cursor_to_name(b"src");
    a.active = 1;
    let p = a.panel_mut();
    p.ensure_sorted();
    for name in [&b"mountains.jpg"[..], b"screenshot-01.png"] {
        p.cursor_to_name(name);
        p.toggle_mark(false);
    }
    p.cursor_to_name(b"talk-slides.pdf");
    a
}

fn dialog_scene(palette: Palette) -> App {
    use manycommander::fsops::question::{Question, Side};
    use manycommander::ui::dialog::Dialog;
    let mut a = scene(palette);
    let side = |size, sec| Side {
        kind: Kind::File,
        size,
        mtime: Ts { sec, nsec: 0 },
        readonly: false,
    };
    let q = Question::FileExists {
        path: PathBuf::from("/home/you/Pictures/mountains.jpg"),
        src: side(5_872_014, at(21, 18, 3)),
        dst: side(4_310_556, at(3, 9, 47)),
        dst_is_symlink: false,
    };
    let (tx, _rx) = std::sync::mpsc::channel();
    a.dialog = Some(Dialog::question(q, tx));
    a
}

// ---- SVG ------------------------------------------------------------------------------------

/// Resolved colours: the palette with the ANSI names mapped to its keys.
struct Colors<'a> {
    p: &'a Palette,
    fg: Rgb,
    bg: Rgb,
}

impl Colors<'_> {
    fn new(p: &Palette) -> Colors<'_> {
        let dark = p.mode.as_deref() != Some("light");
        let fg = p.get("foreground").unwrap_or(if dark {
            Rgb(220, 220, 220)
        } else {
            Rgb(30, 30, 30)
        });
        let bg = p.get("background").unwrap_or(if dark {
            Rgb(20, 20, 20)
        } else {
            Rgb(250, 250, 250)
        });
        Colors { p, fg, bg }
    }

    fn resolve(&self, c: Color, reset: Rgb) -> Rgb {
        let key = |k: &str, d: Rgb| self.p.get(k).unwrap_or(d);
        match c {
            Color::Reset => reset,
            Color::Rgb(r, g, b) => Rgb(r, g, b),
            Color::Black => key("dark_background", self.bg),
            Color::White | Color::Gray => key("bright_foreground", self.fg),
            Color::DarkGray => key("dark_foreground", self.fg),
            Color::Red => key("red", self.fg),
            Color::Green => key("green", self.fg),
            Color::Yellow => key("yellow", self.fg),
            Color::Blue => key("blue", self.fg),
            Color::Magenta => key("magenta", self.fg),
            Color::Cyan => key("cyan", self.fg),
            Color::LightRed => key("bright_red", self.fg),
            Color::LightGreen => key("bright_green", self.fg),
            Color::LightYellow => key("bright_yellow", self.fg),
            Color::LightBlue => key("bright_blue", self.fg),
            Color::LightMagenta => key("bright_magenta", self.fg),
            Color::LightCyan => key("bright_cyan", self.fg),
            Color::Indexed(_) => reset,
        }
    }
}

fn hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.0, c.1, c.2)
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Look {
    fg: Rgb,
    bg: Rgb,
    bold: bool,
    italic: bool,
    underline: bool,
    dim: bool,
}

/// Which arms of a box-drawing glyph leave the cell centre: left, right, up, down.
fn arms(s: &str) -> Option<[bool; 4]> {
    Some(match s {
        "─" | "━" => [true, true, false, false],
        "│" | "┃" => [false, false, true, true],
        "┌" | "╭" => [false, true, false, true],
        "┐" | "╮" => [true, false, false, true],
        "└" | "╰" => [false, true, true, false],
        "┘" | "╯" => [true, false, true, false],
        "├" => [false, true, true, true],
        "┤" => [true, false, true, true],
        "┬" => [true, true, false, true],
        "┴" => [true, true, true, false],
        "┼" => [true, true, true, true],
        _ => return None,
    })
}

fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

/// Assigns short CSS class names to colours in first-use order: `c<n>` fills,
/// `s<n>` strokes.
#[derive(Default)]
struct Classes {
    colors: Vec<Rgb>,
    stroked: Vec<bool>,
}

impl Classes {
    fn fill(&mut self, c: Rgb) -> usize {
        match self.colors.iter().position(|&x| x == c) {
            Some(i) => i,
            None => {
                self.colors.push(c);
                self.stroked.push(false);
                self.colors.len() - 1
            }
        }
    }

    fn stroke(&mut self, c: Rgb) -> usize {
        let k = self.fill(c);
        self.stroked[k] = true;
        k
    }

    fn css(&self) -> String {
        let mut css = String::new();
        for (k, c) in self.colors.iter().enumerate() {
            let _ = write!(css, ".c{k}{{fill:{}}}", hex(*c));
            if self.stroked[k] {
                let _ = write!(css, ".s{k}{{stroke:{}}}", hex(*c));
            }
        }
        css
    }
}

fn svg(buf: &Buffer, cursor: Option<(u16, u16)>, colors: &Colors, title: &str) -> String {
    let area = buf.area;
    let (w, h) = (area.width as i32 * CW, area.height as i32 * CH);
    let mut classes = Classes::default();
    let mut rects = String::new();
    let mut texts = String::new();
    // Border segments per stroke class, horizontal and vertical: `(line, from, to)`.
    type Segs = Vec<(i32, i32, i32)>;
    let mut segs: BTreeMap<usize, (Segs, Segs)> = BTreeMap::new();

    for y in 0..area.height {
        let mut cells: Vec<(u16, String, Look, usize)> = Vec::new();
        let mut x = 0;
        while x < area.width {
            let c = &buf[(x, y)];
            let m = c.modifier;
            let mut fg = colors.resolve(c.fg, colors.fg);
            let mut bg = colors.resolve(c.bg, colors.bg);
            if m.contains(Modifier::REVERSED) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let sym = c.symbol();
            let width = sym.width().max(1);
            let look = Look {
                fg,
                bg,
                bold: m.contains(Modifier::BOLD),
                italic: m.contains(Modifier::ITALIC),
                underline: m.contains(Modifier::UNDERLINED),
                dim: m.contains(Modifier::DIM),
            };
            cells.push((x, sym.to_string(), look, width));
            x += width as u16;
        }
        let top = y as i32 * CH;
        // Background runs.
        let mut i = 0;
        while i < cells.len() {
            let bg = cells[i].2.bg;
            let start = cells[i].0 as i32;
            let mut end = start;
            while i < cells.len() && cells[i].2.bg == bg {
                end = cells[i].0 as i32 + cells[i].3 as i32;
                i += 1;
            }
            if bg != colors.bg {
                let k = classes.fill(bg);
                let _ = writeln!(
                    rects,
                    r#"<rect class="c{k}" x="{}" y="{top}" width="{}" height="{CH}"/>"#,
                    start * CW,
                    (end - start) * CW
                );
            }
        }
        // Box-drawing glyphs become strokes; everything else is text.
        let mut plain = Vec::with_capacity(cells.len());
        for (cx, sym, look, width) in cells {
            if let Some([l, r, u, d]) = arms(&sym) {
                let k = classes.stroke(look.fg);
                let (x0, cxm, x1) = (
                    cx as i32 * CW,
                    cx as i32 * CW + CW / 2,
                    (cx as i32 + 1) * CW,
                );
                let (y0, cym, y1) = (top, top + CH / 2, top + CH);
                let (hs, vs) = segs.entry(k).or_default();
                if l {
                    hs.push((cym, x0, cxm));
                }
                if r {
                    hs.push((cym, cxm, x1));
                }
                if u {
                    vs.push((cxm, y0, cym));
                }
                if d {
                    vs.push((cxm, cym, y1));
                }
                plain.push((
                    cx,
                    " ".to_string(),
                    Look {
                        underline: false,
                        ..look
                    },
                    width,
                ));
            } else {
                plain.push((cx, sym, look, width));
            }
        }
        // Text runs of one style.
        let mut i = 0;
        while i < plain.len() {
            let look = plain[i].2;
            let start = plain[i].0 as i32;
            let mut s = String::new();
            let mut cells_w = 0;
            while i < plain.len() && same_text_style(&plain[i].2, &look) {
                s.push_str(&plain[i].1);
                cells_w += plain[i].3 as i32;
                i += 1;
            }
            // Leading and trailing blanks carry no ink.
            let lead = s.len() - s.trim_start_matches(' ').len();
            let trimmed = s.trim_matches(' ');
            if trimmed.is_empty() && !look.underline {
                continue;
            }
            let (x0, tw, text) = if look.underline {
                (start, cells_w, s.as_str())
            } else {
                let tw = trimmed.width() as i32;
                (start + lead as i32, tw, trimmed)
            };
            let k = classes.fill(look.fg);
            let mut cls = format!("c{k}");
            if look.bold {
                cls.push_str(" B");
            }
            if look.italic {
                cls.push_str(" I");
            }
            if look.underline {
                cls.push_str(" U");
            }
            if look.dim {
                cls.push_str(" D");
            }
            let _ = writeln!(
                texts,
                r#"<text class="{cls}" x="{}" y="{}" textLength="{}" lengthAdjust="spacingAndGlyphs">{}</text>"#,
                x0 * CW,
                top + BASELINE,
                tw * CW,
                esc(text)
            );
        }
    }

    let mut paths = String::new();
    for (k, (hs, vs)) in segs {
        let _ = write!(paths, r#"<path class="s{k}" d=""#);
        for (y, x0, x1) in merge(hs) {
            let _ = write!(paths, "M{x0} {y}H{x1}");
        }
        for (x, y0, y1) in merge(vs) {
            let _ = write!(paths, "M{x} {y0}V{y1}");
        }
        paths.push_str("\"/>\n");
    }
    let caret = cursor.map(|(cx, cy)| {
        let k = classes.fill(colors.fg);
        format!(
            "<rect class=\"c{k}\" x=\"{}\" y=\"{}\" width=\"{CW}\" height=\"{CH}\"/>\n",
            cx as i32 * CW,
            cy as i32 * CH
        )
    });

    let mut css = format!(
        "text{{font-family:{FONTS};font-size:{FONT_SIZE}px;white-space:pre;\
         font-variant-ligatures:none}}\
         path{{fill:none;stroke-width:1.2}}.B{{font-weight:700}}.I{{font-style:italic}}\
         .U{{text-decoration:underline}}.D{{opacity:.6}}"
    );
    css.push_str(&classes.css());
    let mut out = String::new();
    let _ = writeln!(
        out,
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" xml:space="preserve">"#
    );
    let _ = writeln!(out, "<title>{}</title>", esc(title));
    let _ = writeln!(out, "<style>{css}</style>");
    let _ = writeln!(
        out,
        r#"<rect width="{w}" height="{h}" fill="{}"/>"#,
        hex(colors.bg)
    );
    out.push_str(&rects);
    out.push_str(caret.as_deref().unwrap_or(""));
    out.push_str(&paths);
    out.push_str(&texts);
    out.push_str("</svg>\n");
    out
}

fn same_text_style(a: &Look, b: &Look) -> bool {
    a.fg == b.fg
        && a.bold == b.bold
        && a.italic == b.italic
        && a.underline == b.underline
        && a.dim == b.dim
}

/// Sorts and joins touching segments `(line, from, to)`.
fn merge(mut segs: Vec<(i32, i32, i32)>) -> Vec<(i32, i32, i32)> {
    segs.sort_unstable();
    let mut out: Vec<(i32, i32, i32)> = Vec::new();
    for s in segs {
        match out.last_mut() {
            Some(l) if l.0 == s.0 && l.2 >= s.1 => l.2 = l.2.max(s.2),
            _ => out.push(s),
        }
    }
    out
}

fn render(app: &mut App, colors: &Colors, title: &str) -> String {
    let mut term = Terminal::new(TestBackend::new(COLS, ROWS)).expect("test backend");
    term.draw(|f| manycommander::ui::draw(app, f))
        .expect("draw");
    let cursor = if app.dialog.is_none() {
        term.backend_mut()
            .get_cursor_position()
            .ok()
            .map(|p| (p.x, p.y))
    } else {
        None
    };
    svg(term.backend().buffer(), cursor, colors, title)
}

/// The palette keys `themes.json` carries for the site, each with its fallback chain. A
/// chain that ends in `background` falls back to the base background, any other to the
/// base foreground.
const SITE_COLORS: &[(&str, &[&str])] = &[
    ("background", &["background"]),
    ("foreground", &["foreground"]),
    ("accent", &["blue", "foreground"]),
    ("muted", &["dark_foreground", "foreground"]),
    ("selection", &["lighter_background", "background"]),
    ("dark_background", &["background"]),
    ("lighter_background", &["selection", "background"]),
    ("dark_foreground", &["foreground"]),
    ("light_foreground", &["foreground"]),
    ("bright_foreground", &["foreground"]),
    ("red", &["foreground"]),
    ("green", &["foreground"]),
    ("yellow", &["foreground"]),
    ("cyan", &["foreground"]),
    ("magenta", &["foreground"]),
    ("blue", &["foreground"]),
];

/// `tokyo-night` -> `Tokyo Night`.
fn display_name(name: &str) -> String {
    name.split('-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_uppercase().chain(c).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => {
                let _ = write!(o, "\\u{:04x}", c as u32);
            }
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn write(path: &Path, text: &str) -> Result<(), String> {
    std::fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    println!("wrote {} ({} bytes)", path.display(), text.len());
    Ok(())
}

fn main() -> Result<(), String> {
    let mut args = std::env::args_os().skip(1);
    let themes = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| "/usr/share/omarchy/themes".into());
    let out = args
        .next()
        .map(PathBuf::from)
        .unwrap_or_else(|| "site/static/screens".into());

    let mut palettes = BTreeMap::new();
    let dir = std::fs::read_dir(&themes).map_err(|e| format!("{}: {e}", themes.display()))?;
    for d in dir {
        let d = d.map_err(|e| format!("{}: {e}", themes.display()))?;
        let file = d.path().join("colors.toml");
        if !file.is_file() {
            continue;
        }
        let name = d
            .file_name()
            .into_string()
            .map_err(|n| format!("theme name is not UTF-8: {n:?}"))?;
        palettes.insert(name, Palette::load(&file)?);
    }
    if palettes.is_empty() {
        return Err(format!("no */colors.toml under {}", themes.display()));
    }
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;

    let mut index = String::from("[\n");
    for (i, (name, p)) in palettes.iter().enumerate() {
        let colors = Colors::new(p);
        let title = format!("manycommander in the {} theme", display_name(name));
        let text = render(&mut scene(p.clone()), &colors, &title);
        write(&out.join(format!("{name}.svg")), &text)?;
        let mode = p.mode.as_deref().unwrap_or("dark");
        let site_colors = SITE_COLORS
            .iter()
            .map(|(key, chain)| {
                let c = std::iter::once(key)
                    .chain(chain.iter())
                    .find_map(|k| p.get(k))
                    .unwrap_or(if chain.contains(&"background") {
                        colors.bg
                    } else {
                        colors.fg
                    });
                format!("{}: \"{}\"", json_str(key), hex(c))
            })
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(
            index,
            "  {{\"name\": {}, \"title\": {}, \"mode\": {}, \"colors\": {{{site_colors}}}}}{}",
            json_str(name),
            json_str(&display_name(name)),
            json_str(mode),
            if i + 1 < palettes.len() { "," } else { "" }
        );
    }
    index.push_str("]\n");
    write(&out.join("themes.json"), &index)?;

    let p = palettes
        .get(DEFAULT_THEME)
        .ok_or_else(|| format!("{DEFAULT_THEME} is missing under {}", themes.display()))?;
    let colors = Colors::new(p);
    let text = render(
        &mut dialog_scene(p.clone()),
        &colors,
        "manycommander asking before it overwrites a file",
    );
    write(&out.join("dialog.svg"), &text)?;
    Ok(())
}
