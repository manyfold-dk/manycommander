//! Job planning and execution: copy, move, mkdir, trash, delete (design section 4).
//!
//! Only `sys` may contain `unsafe`; every other module here forbids it.

pub mod failpoints;
pub mod identity;
pub mod sys;
