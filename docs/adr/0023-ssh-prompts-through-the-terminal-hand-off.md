# ADR 0023: SSH prompts go through the terminal hand-off, and ssh runs in its own process group

## Status

Accepted -- 2026-09-28.

## Context

A connection may need the user: a key passphrase, a password, the confirmation of a new
host key, or a FIDO touch. ssh asks on `/dev/tty`. manycommander, meanwhile, holds the
terminal in raw mode on the alternate screen, and its input thread reads every key.

manycommander already hands the terminal to a child for F3, F4 and the command line
([TDR-1 6](../tdr/0001-core.md#6-command-line-and-hand-off)). That child runs in
manycommander's process group, so the terminal's `Ctrl+C` reaches it. An `ssh` child in the
same group would also receive a `Ctrl+C` typed into `less` during a later F3, and every
session would die with it.

## Decision

A connect runs inside the existing hand-off, and `ssh` runs in its own process group:

1. manycommander hands the terminal off as for F3 (the input thread parks, the alternate
   screen is left, the terminal returns to cooked mode) and prints
   "connecting to <host> ... (Ctrl+C cancels)".
2. It spawns `ssh` with `process_group(0)`, makes ssh's group the terminal's foreground
   group with `tcsetpgrp`, sends it `SIGCONT`, and sends `SSH_FXP_INIT`. ssh prompts
   directly on the terminal; `Ctrl+C` reaches only ssh.
3. The UI thread polls ssh's stdout for `SSH_FXP_VERSION` every 50 ms, and between polls
   checks ssh's group with `waitid(P_PGID, WEXITED | WSTOPPED | WNOHANG | WNOWAIT)`, because
   a stopped ssh does not close its stdout.
4. On `VERSION`, manycommander takes the foreground back with `SIGTTOU` blocked on the
   calling thread, and the TUI resumes. After that the UI thread never calls the session.
5. When ssh exits or stops first, manycommander kills ssh's group with `SIGKILL`, reaps it,
   takes the foreground back and shows `[connection failed] press Enter to return` under
   ssh's own messages.

## Rationale

- manycommander never sees a password or a passphrase, and never answers a host-key
  question on the user's behalf (R-6).
- The prompts look exactly as they do in a shell, including FIDO touch messages and
  host-key fingerprints.
- The hand-off is already built and tested (A-UI-3); the connect adds only the process-group
  handling, which A-SF-6 tests in a pty with `Ctrl+C` and `Ctrl+Z` at a password prompt.

## Consequences

- Every connect switches the screen, even when no prompt is needed.
- `pthread_sigmask` on the calling thread needs the `nix` crate (feature `signal`): neither
  std nor rustix offers a safe call for it. `tcsetpgrp`, `waitid` and `kill` come from
  rustix. No `unsafe` pre-exec code is used; the `SIGCONT` after `tcsetpgrp` clears a stop
  that ssh took before it owned the terminal.
- An ssh that wants the terminal after the connect (for example `ControlMaster ask`) stops
  in the background; the operation waits until cancelled, and the F1 help names the cause.
- When a `ProxyCommand` or `ProxyJump` child shares ssh's group, `Ctrl+Z` at a password
  prompt can stop the proxy but not ssh. That is OpenSSH's behaviour; the F1 help says that
  `Ctrl+C` or a second `Ctrl+Z` ends it.

## Alternatives considered

| Alternative | Why not |
|---|---|
| An in-TUI askpass dialog | A dialog of its own to build and test; the hand-off already works. It would remove the screen switch, and stays possible later |
| A first attempt with `BatchMode=yes`, falling back to the hand-off | Doubles the connection setup whenever a prompt is needed, and hides FIDO touch messages |
| ssh in manycommander's process group | A `Ctrl+C` during a later hand-off would end every session |
| `setpgid` and `tcsetpgrp` in a `pre_exec` hook | Needs `unsafe` outside `fsops/sys.rs` (NFR-SEC) |

## References

- [TDR-3 5.2](../tdr/0003-archives-quick-view-and-sftp.md#52-transport)
- [TDR-3 appendix A, E-17, E-18, E-20](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
- [ADR 0022](0022-sftp-client-over-the-system-ssh.md)
