#![forbid(unsafe_code)]
//! Theming (TDR-1 7): `colors.toml` parsing, the role table, colour depth and the
//! reload sources.

pub mod palette;
pub mod roles;
pub mod watch;

pub use palette::Palette;
pub use roles::{Depth, Theme};
