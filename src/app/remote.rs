#![forbid(unsafe_code)]
//! SFTP in the app (P3 5.1, 5.2): `cd sftp://...` on the command line connects. The address
//! grammar and `sftp.ssh` are checked here, before anything is spawned; the runtime then runs
//! the connect hand-off. After `SSH_FXP_VERSION` the UI thread never calls the session: a
//! listing thread reads the login directory and reports it. Until remote panels land (T6),
//! that report in the status line is what a connect shows.

use super::App;
use super::event::Effect;
use crate::remote::RemoteMsg;
use crate::remote::transport::SshCommand;
use crate::remote::url;
use crate::ui::text::escaped;

impl App {
    /// `cd sftp://...` (P3 5.1).
    pub(super) fn connect(&mut self, text: &[u8]) -> Vec<Effect> {
        let addr = match url::parse(text) {
            Ok(a) => a,
            Err(e) => {
                self.warn(e);
                return Vec::new();
            }
        };
        let cmd = match SshCommand::from_setting(self.config.sftp.ssh.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.warn(e);
                return Vec::new();
            }
        };
        vec![Effect::Connect(addr, cmd)]
    }

    pub(super) fn on_remote(&mut self, m: RemoteMsg) -> Vec<Effect> {
        match m {
            RemoteMsg::Failed { address, message } => {
                self.warn(format!("{address}: connection failed: {message}"));
            }
            RemoteMsg::Home {
                address,
                home: Ok(home),
                reused,
            } => {
                let how = if reused { "session open" } else { "connected" };
                self.say(format!(
                    "{address}: {how}, home {}; remote panels are not available here yet",
                    escaped(&home)
                ));
            }
            RemoteMsg::Home {
                address,
                home: Err(e),
                ..
            } => self.warn(format!("{address}: {e}")),
            RemoteMsg::Lost { address, message } => {
                self.warn(format!("{address}: connection lost: {message}"));
            }
        }
        Vec::new()
    }
}
