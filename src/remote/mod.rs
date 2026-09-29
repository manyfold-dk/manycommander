#![forbid(unsafe_code)]
//! SFTP (P3 5): an own synchronous SFTP version 3 client over the system `ssh -s <host>
//! sftp` (D-3). No async runtime and no crypto crates: OpenSSH keeps `ssh_config`, the
//! agent, keys, `known_hosts` and the host-key policy, and manycommander never weakens them
//! (R-6).
//!
//! - [`url`]: the `sftp://` address grammar (P3 5.1).
//! - [`proto`]: the codec, with every length and count checked before any allocation
//!   (P3 5.3).
//! - [`session`]: the reader thread, reply slots, cancel with the 2 s drain window, the
//!   pipelined windows and session loss (P3 2.5, 5.3).
//! - [`transport`]: the argv and its fixed options, ssh's own process group, the stderr
//!   tail, and the connect hand-off (P3 5.2).
//! - [`provider`]: a session as a place: listings, the symlink pass, the login directory,
//!   free space, and checked file reads (P3 5.4, 5.5).
//! - [`tree`]: downloads, a copy with a remote origin and its scan (P3 5.5).
//! - [`pool`]: at most four open sessions, least recently used out (P3 5.7).
//!
//! The UI thread performs a connect itself, inside the terminal hand-off; it is the one
//! place that drives a session before `SSH_FXP_VERSION`. After that the UI thread never
//! calls a session: listing threads and the job worker do.

pub mod pool;
pub mod proto;
pub mod provider;
pub mod session;
pub mod transport;
pub mod tree;
pub mod url;

pub use provider::RemoteProvider;
pub use session::{Lost, Session, SftpError};

/// What the remote side tells the UI thread (P3 2.5).
#[derive(Debug)]
pub enum RemoteMsg {
    /// A connect ended without a session; the hand-off already showed ssh's messages.
    Failed { address: String, message: String },
    /// A connect succeeded: the session for `target`, past `SSH_FXP_VERSION`.
    Connected {
        target: crate::provider::Target,
        session: Session,
    },
    /// The session ended (P3 5.3): the reason, or ssh's last stderr line.
    Lost { address: String, message: String },
}
