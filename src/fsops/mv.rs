#![forbid(unsafe_code)]
//! Move and rename (F6, design 4.8). Implemented in T4.

use super::job::{JobVerb, Report};
use super::question::Interaction;
use super::sys::Sys;
use std::ffi::OsString;
use std::path::Path;

pub fn move_job(
    _sys: &Sys,
    _ui: &mut dyn Interaction,
    _src: &Path,
    _names: &[OsString],
    _dst: &Path,
) -> Report {
    Report::refused(JobVerb::Move, "not implemented yet")
}
