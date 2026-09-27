#![forbid(unsafe_code)]
//! Permanent delete (Shift+F8, design 4.11). Implemented in T5.

use super::job::{JobVerb, Report};
use super::question::Interaction;
use super::sys::Sys;
use std::ffi::OsString;
use std::path::Path;

pub fn delete_job(
    _sys: &Sys,
    _ui: &mut dyn Interaction,
    _dir: &Path,
    _names: &[OsString],
) -> Report {
    Report::refused(JobVerb::Delete, "not implemented yet")
}
