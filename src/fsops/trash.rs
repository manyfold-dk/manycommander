#![forbid(unsafe_code)]
//! Trash (F8, design 4.10). Implemented in T5.

use super::job::{JobVerb, Report};
use super::question::Interaction;
use super::sys::Sys;
use std::ffi::OsString;
use std::path::Path;

pub fn trash_job(
    _sys: &Sys,
    _ui: &mut dyn Interaction,
    _dir: &Path,
    _names: &[OsString],
) -> Report {
    Report::refused(JobVerb::Trash, "not implemented yet")
}
