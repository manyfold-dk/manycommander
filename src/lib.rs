//! manycommander: a dual-pane, keyboard-driven terminal file manager.
//!
//! The library holds everything that is testable without a terminal. The binary wires the
//! terminal and the event loop. `unsafe` is confined to `fsops::sys` (NFR-SEC); every other
//! module starts with `#![forbid(unsafe_code)]`. There is deliberately no `forbid` at the
//! crate root, because `sys` could not override it.

pub mod app;
pub mod cmdline;
pub mod compare;
pub mod config;
pub mod dirs;
pub mod find;
pub mod fsops;
pub mod panel;
pub mod rename;
pub mod theme;
pub mod ui;
