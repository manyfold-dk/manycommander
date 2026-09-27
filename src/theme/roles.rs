#![forbid(unsafe_code)]
//! UI roles (design 7.1) and colour depth (7.4).
//!
//! Each role is defined once, as a fallback chain of palette keys plus a named ANSI colour
//! for when the chain finds nothing, the palette is absent, or the terminal has no
//! truecolor. `NO_COLOR` drops colour entirely; bold, reverse video and the marker glyph
//! keep cursor and marks distinguishable (NFR-TERM).

use super::palette::{Palette, Rgb};
use ratatui::style::{Color, Modifier, Style};
use std::ffi::OsStr;

/// How colours reach the terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Depth {
    /// `COLORTERM` is `truecolor` or `24bit`: roles use RGB.
    TrueColor,
    /// Every role uses its named ANSI fallback; there is no 256-colour approximation.
    Ansi,
    /// `NO_COLOR`: no colour attributes at all.
    NoColor,
}

impl Depth {
    pub fn detect(colorterm: Option<&OsStr>, no_color: Option<&OsStr>) -> Depth {
        // NO_COLOR applies when it is set to a non-empty value (no-color.org).
        if no_color.is_some_and(|v| !v.is_empty()) {
            return Depth::NoColor;
        }
        match colorterm.and_then(|c| c.to_str()) {
            Some("truecolor" | "24bit") => Depth::TrueColor,
            _ => Depth::Ansi,
        }
    }

    pub fn from_env() -> Depth {
        Depth::detect(
            std::env::var_os("COLORTERM").as_deref(),
            std::env::var_os("NO_COLOR").as_deref(),
        )
    }
}

/// The marker glyph of a marked entry.
pub const MARK_GLYPH: &str = "▸";

/// Every styled role of the UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    pub background: Style,
    pub normal: Style,
    pub directory: Style,
    pub executable: Style,
    pub symlink: Style,
    pub broken_symlink: Style,
    pub hidden: Style,
    pub marked: Style,
    pub cursor_active: Style,
    pub cursor_inactive: Style,
    pub border_active: Style,
    pub border_inactive: Style,
    pub metadata: Style,
    pub dialog: Style,
    pub dialog_border: Style,
    pub error: Style,
    pub warning: Style,
    pub fkey_label: Style,
    pub fkey_number: Style,
    pub prompt: Style,
    /// Escaped control characters and invalid UTF-8 bytes in names (design 3.2).
    pub escaped: Style,
    pub depth: Depth,
}

impl Default for Theme {
    fn default() -> Theme {
        Theme::build(None, Depth::Ansi, false)
    }
}

fn rgb(c: Rgb) -> Color {
    Color::Rgb(c.0, c.1, c.2)
}

impl Theme {
    /// Builds every role from a palette (or none), a colour depth and the
    /// `paint_background` setting.
    pub fn build(p: Option<&Palette>, depth: Depth, paint_background: bool) -> Theme {
        // A colour for a role: the palette chain in truecolor, else the ANSI fallback.
        let c = |chain: &[&str], ansi: Color| -> Option<Color> {
            match depth {
                Depth::NoColor => None,
                Depth::Ansi => Some(ansi),
                Depth::TrueColor => Some(p.and_then(|p| p.chain(chain)).map(rgb).unwrap_or(ansi)),
            }
        };
        let fg = |chain: &[&str], ansi: Color| match c(chain, ansi) {
            Some(col) => Style::new().fg(col),
            None => Style::new(),
        };
        let bg = |s: Style, chain: &[&str], ansi: Color| match c(chain, ansi) {
            Some(col) => s.bg(col),
            None => s,
        };
        let bold = Modifier::BOLD;

        let background = if paint_background && depth == Depth::TrueColor {
            bg(Style::new(), &["background"], Color::Reset)
        } else {
            Style::new().bg(Color::Reset)
        };
        let background = if depth == Depth::NoColor {
            Style::new()
        } else {
            background
        };

        let mut cursor_active = bg(
            fg(&["background"], Color::Black),
            &["accent", "blue"],
            Color::Blue,
        );
        let mut cursor_inactive = bg(
            Style::new(),
            &["selection", "lighter_background"],
            Color::DarkGray,
        );
        if depth == Depth::NoColor {
            cursor_active = Style::new().add_modifier(Modifier::REVERSED);
            cursor_inactive = Style::new().add_modifier(Modifier::UNDERLINED);
        }
        Theme {
            background,
            normal: fg(&["foreground"], Color::Reset),
            directory: fg(&["bright_foreground"], Color::White).add_modifier(bold),
            executable: fg(&["green"], Color::Green),
            symlink: fg(&["cyan"], Color::Cyan),
            broken_symlink: fg(&["red"], Color::Red),
            hidden: fg(&["dark_foreground"], Color::DarkGray),
            marked: fg(&["yellow"], Color::Yellow).add_modifier(bold),
            cursor_active,
            cursor_inactive,
            border_active: fg(&["accent", "blue"], Color::Blue),
            border_inactive: fg(&["muted", "dark_foreground"], Color::DarkGray),
            metadata: fg(&["light_foreground", "foreground"], Color::Reset),
            dialog: bg(
                fg(&["foreground"], Color::Reset),
                &["dark_background", "background"],
                Color::Reset,
            ),
            dialog_border: fg(&["accent", "blue"], Color::Blue),
            error: fg(&["red"], Color::Red).add_modifier(if depth == Depth::NoColor {
                bold
            } else {
                Modifier::empty()
            }),
            warning: fg(&["yellow"], Color::Yellow),
            fkey_label: bg(
                fg(&["foreground"], Color::Black),
                &["lighter_background", "selection"],
                Color::Cyan,
            ),
            fkey_number: fg(&["accent", "blue"], Color::Reset),
            prompt: fg(&["accent", "blue"], Color::Blue),
            escaped: fg(&["magenta"], Color::Magenta).add_modifier(if depth == Depth::NoColor {
                Modifier::UNDERLINED
            } else {
                Modifier::empty()
            }),
            depth,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pal(s: &str) -> Palette {
        Palette::parse(s).unwrap()
    }

    #[test]
    fn depth_detection() {
        let t = |ct: Option<&str>, nc: Option<&str>| {
            Depth::detect(ct.map(OsStr::new), nc.map(OsStr::new))
        };
        assert_eq!(t(Some("truecolor"), None), Depth::TrueColor);
        assert_eq!(t(Some("24bit"), None), Depth::TrueColor);
        assert_eq!(t(Some("256"), None), Depth::Ansi);
        assert_eq!(t(None, None), Depth::Ansi);
        assert_eq!(t(Some("truecolor"), Some("1")), Depth::NoColor);
        assert_eq!(t(Some("truecolor"), Some("")), Depth::TrueColor);
    }

    #[test]
    fn roles_use_palette_in_truecolor() {
        let p = pal(
            "accent = \"#010203\"\nbackground = \"#0a0b0c\"\nbright_foreground = \"#ffffff\"\n",
        );
        let th = Theme::build(Some(&p), Depth::TrueColor, false);
        assert_eq!(th.border_active.fg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(th.cursor_active.bg, Some(Color::Rgb(1, 2, 3)));
        assert_eq!(th.cursor_active.fg, Some(Color::Rgb(10, 11, 12)));
        assert_eq!(th.directory.fg, Some(Color::Rgb(255, 255, 255)));
        assert!(th.directory.add_modifier.contains(Modifier::BOLD));
        assert_eq!(
            th.background.bg,
            Some(Color::Reset),
            "the terminal owns the background"
        );
        let painted = Theme::build(Some(&p), Depth::TrueColor, true);
        assert_eq!(painted.background.bg, Some(Color::Rgb(10, 11, 12)));
    }

    #[test]
    fn ansi_and_no_color() {
        let p = pal("accent = \"#010203\"\n");
        let th = Theme::build(Some(&p), Depth::Ansi, true);
        assert_eq!(th.border_active.fg, Some(Color::Blue));
        assert_eq!(th.background.bg, Some(Color::Reset));
        let nc = Theme::build(Some(&p), Depth::NoColor, false);
        assert_eq!(nc.border_active.fg, None);
        assert!(nc.cursor_active.add_modifier.contains(Modifier::REVERSED));
        assert!(nc.marked.add_modifier.contains(Modifier::BOLD));
        assert_eq!(nc.marked.fg, None);
    }
}
