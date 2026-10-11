#![forbid(unsafe_code)]
//! SFTP (TDR-3 5): an own synchronous SFTP version 3 client over the system `ssh -s <host>
//! sftp` (TDR-3 D-3). No async runtime and no crypto crates: OpenSSH keeps `ssh_config`, the
//! agent, keys, `known_hosts` and the host-key policy, and manycommander never weakens them
//! (R-6).
//!
//! - [`url`]: the `sftp://` address grammar (TDR-3 5.1).
//! - [`proto`]: the codec, with every length and count checked before any allocation
//!   (TDR-3 5.3).
//! - [`session`]: the reader thread, reply slots, cancel with the 2 s drain window, the
//!   pipelined windows and session loss (TDR-3 2.5, 5.3).
//! - [`transport`]: the argv and its fixed options, ssh's own process group, the stderr
//!   tail, and the connect hand-off (TDR-3 5.2).
//! - [`provider`]: a session as a place: listings, the symlink pass, the login directory,
//!   free space, and checked file reads (TDR-3 5.4, 5.5).
//! - [`tree`]: downloads, a copy with a remote origin and its scan (TDR-3 5.5), and the
//!   download move that keeps its remote sources (R-4).
//! - [`put`]: uploads, F5 and F6 to a server and the F4 write-back: the hard-link commit,
//!   the direct-write mode and the atomic replace (R-1, R-2, R-4; TDR-3 5.6).
//! - [`rename`]: renames and moves within one session, and new directories (R-2, TDR-3 5.6).
//! - [`delete`]: Shift+F8 on a server; there is no remote trash (R-5, TDR-3 5.6).
//! - [`pool`]: at most four open sessions, least recently used out (TDR-3 5.7).
//!
//! The UI thread performs a connect itself, inside the terminal hand-off; it is the one
//! place that drives a session before `SSH_FXP_VERSION`. After that the UI thread never
//! calls a session: listing threads and the job worker do.

pub mod delete;
pub mod pool;
pub mod proto;
pub mod provider;
pub mod put;
pub mod rename;
pub mod session;
pub mod transport;
pub mod tree;
pub mod url;

pub use provider::RemoteProvider;
pub use session::{Lost, Session, SftpError};

/// What the remote side tells the UI thread (TDR-3 2.5).
#[derive(Debug)]
pub enum RemoteMsg {
    /// A connect ended without a session; the hand-off already showed ssh's messages.
    Failed { address: String, message: String },
    /// A connect succeeded: the session for `target`, past `SSH_FXP_VERSION`.
    Connected {
        target: crate::provider::Target,
        session: Session,
    },
    /// The session ended (TDR-3 5.3): the reason, or ssh's last stderr line.
    Lost { address: String, message: String },
}

/// What the server's error question decided, for the engines that change a server.
pub(crate) enum Ask {
    Retry,
    /// The entry fails with the text.
    Fail(String),
    Stop,
}

/// The server's error question of one job (TDR-3 5.6: TDR-1's questions), with the job's "skip
/// all" answers by message.
#[derive(Default)]
pub(crate) struct ServerErrors {
    skip: std::collections::HashSet<String>,
}

impl ServerErrors {
    /// The question for `e` at `path`, or the entry's end without one: a lost session fails
    /// it and sets `lost` (every later entry fails with it, I-7), a cancel stops the job.
    pub(crate) fn ask(
        &mut self,
        rep: &mut crate::fsops::question::Reporter,
        lost: &mut bool,
        path: &std::path::Path,
        op: &'static str,
        e: &SftpError,
    ) -> Ask {
        use crate::fsops::question::{Answer, Question};
        match e {
            SftpError::Lost => {
                *lost = true;
                Ask::Fail(tree::LOST.into())
            }
            SftpError::Cancelled => Ask::Stop,
            SftpError::Local(m) => Ask::Fail(format!("{op}: {m}")),
            SftpError::Status { .. } => {
                let message = e.to_string();
                let why = format!("{op}: {message}");
                if self.skip.contains(&message) {
                    return Ask::Fail(why);
                }
                let q = Question::ServerError {
                    path: path.to_path_buf(),
                    op,
                    message: message.clone(),
                };
                match rep.ask(q) {
                    Answer::Retry => Ask::Retry,
                    Answer::SkipAllErrno => {
                        self.skip.insert(message);
                        Ask::Fail(why)
                    }
                    Answer::Cancel => Ask::Stop,
                    _ => Ask::Fail(why),
                }
            }
        }
    }
}

/// `LSTAT` of a server path after the failpoint `fp`: `None` when nothing is there. A lost
/// session sets `lost`.
pub(crate) fn lstat(
    sys: &crate::fsops::sys::Sys,
    fp: &'static str,
    s: &Session,
    path: &[u8],
    cancel: &std::sync::atomic::AtomicBool,
    lost: &mut bool,
) -> Result<Option<proto::Attrs>, SftpError> {
    step(sys, fp)?;
    match s.lstat(path, cancel) {
        Ok(a) => Ok(Some(a)),
        Err(SftpError::Status {
            code: proto::status::NO_SUCH_FILE,
            ..
        }) => Ok(None),
        Err(e) => {
            if e == SftpError::Lost {
                *lost = true;
            }
            Err(e)
        }
    }
}

/// A failpoint on a step of a write on the server (the A-SF-9 sweep): an injected errno is
/// answered as the server answers that errno (`ENOENT`, `EACCES`, `EOPNOTSUPP` and the
/// rest as `SSH_FX_FAILURE`), with the errno's text as the message. Session loss is
/// injected by killing the peer in an `Action::Call`; the request that follows then fails
/// with "connection lost".
pub(crate) fn step(sys: &crate::fsops::sys::Sys, name: &'static str) -> Result<(), SftpError> {
    use proto::status;
    use rustix::io::Errno;
    sys.hit(name).map_err(|e| SftpError::Status {
        code: match e {
            Errno::NOENT => status::NO_SUCH_FILE,
            Errno::ACCESS | Errno::PERM => status::PERMISSION_DENIED,
            Errno::OPNOTSUPP | Errno::NOSYS => status::OP_UNSUPPORTED,
            _ => status::FAILURE,
        },
        message: crate::fsops::walk::errno_text(e),
    })
}
