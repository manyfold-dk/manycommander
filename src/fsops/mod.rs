//! Job planning and execution: copy, move, mkdir, trash, delete (design section 4), links
//! and attributes (P2 8), multi-rename (P2 6.3). The copy engine reads its sources through
//! an `origin::Origin` (P3 2.3).
//!
//! Only `sys` may contain `unsafe`; every other module here forbids it.

pub mod attr;
pub mod copy;
pub mod delete;
pub mod failpoints;
pub mod group;
pub mod identity;
pub mod job;
pub mod link;
pub mod mkdir;
pub mod mv;
pub mod origin;
pub mod plan;
pub mod question;
pub mod rename;
pub mod sys;
pub mod trash;
pub mod walk;
