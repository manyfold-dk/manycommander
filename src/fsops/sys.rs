//! The syscall layer (design section 4). The only module allowed to contain `unsafe`; it is
//! expected to need none, because `rustix` provides safe wrappers for every call.
