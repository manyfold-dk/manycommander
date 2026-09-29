#![forbid(unsafe_code)]
//! SFTP (P3 5): an own synchronous SFTP version 3 client over the system `ssh -s <host>
//! sftp` (D-3). No async runtime and no crypto crates: OpenSSH keeps `ssh_config`, the
//! agent, keys, `known_hosts` and the host-key policy, and manycommander never weakens them
//! (R-6).
//!
//! - [`url`]: the `sftp://` address grammar (P3 5.1).
//! - [`proto`]: the codec, with every length and count checked before any allocation
//!   (P3 5.3).

pub mod proto;
pub mod url;
