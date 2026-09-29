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
//!
//! The UI thread performs a connect itself, inside the terminal hand-off; it is the one
//! place that drives a session before `SSH_FXP_VERSION`. After that the UI thread never
//! calls a session: listing threads and the job worker do.

pub mod proto;
pub mod session;
pub mod transport;
pub mod url;

pub use session::{Lost, Session, SftpError};

/// What the remote side tells the UI thread (P3 2.5).
#[derive(Debug)]
pub enum RemoteMsg {
    /// A connect ended without a session; the hand-off already showed ssh's messages.
    Failed { address: String, message: String },
    /// The login directory, read on a listing thread after the connect, or why it could
    /// not be read. `reused`: the connect found the session open. Until remote panels land
    /// (T6) this is what a connect reports.
    Home {
        address: String,
        home: Result<Vec<u8>, String>,
        reused: bool,
    },
    /// The session ended (P3 5.3): the reason, or ssh's last stderr line.
    Lost { address: String, message: String },
}
