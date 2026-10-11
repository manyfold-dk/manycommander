# ADR 0022: SFTP is an own version 3 client over the system ssh

## Status

Accepted -- 2026-09-28.

## Context

Users reach their servers through an OpenSSH setup that has grown over years: `ssh_config`
with `Include` and `Match`, the agent, FIDO keys, certificates, `known_hosts` with hashed,
wildcard, `@revoked` and `@cert-authority` entries, `ProxyJump`, and their own host-key
policy. An SFTP client that re-implements SSH has to re-implement that setup, and every gap
is either a connection that fails or a host-key check that is weaker than the user's.

manycommander has no async runtime ([ADR 0003](0003-threads-and-one-event-channel.md)), and
every new crate is weighed against its supply-chain cost (NFR-SUP).

## Decision

manycommander spawns the user's `ssh` and speaks SFTP version 3
(draft-ietf-secsh-filexfer-02, plus the extensions in OpenSSH's `PROTOCOL` file) over its
stdin and stdout, with an own synchronous client in `src/remote/` that has no `unsafe`. The
argv is fixed:

```text
<program> -oForwardAgent=no -oForwardX11=no -oClearAllForwardings=yes
-oPermitLocalCommand=no -oRequestTTY=no -oRemoteCommand=none -e none
<the other sftp.ssh arguments> [-l <user>] [-p <port>] -s -- <host> sftp
```

The transport rules (R-6):

- The program spawns by argv, never through a shell. Host and user follow `--`.
- The address grammar is strict, because `ssh_config` can expand host and user into
  `ProxyCommand` and `Match exec` shell commands (the CVE-2023-51385 shape).
- The `sftp.ssh` setting is parsed the way ssh parses options. An argument that is or starts
  with `-o`, or is `-A`, `-X`, `-Y`, `-t` or `-e` (which override an `-o` wherever they
  stand), is refused, as are combined flags that contain them, plain words and `--`.
- manycommander never passes an option that relaxes host-key checking or authentication,
  and never answers an ssh prompt itself.

The codec checks every length and count against the bytes left before it allocates, and a
reply above OpenSSH's own maximum, of the wrong type or with an unknown id ends the session.
The write operations (upload, mkdir, rename, delete) build on the same session, codec and
scan as the read operations.

## Rationale

- OpenSSH stays the authority on configuration, authentication and host keys. Whatever the
  user's `ssh` accepts, manycommander accepts, and nothing else.
- The client adds zero crates, no runtime and no cryptography.
- It is fast enough: with 124 KiB page-aligned requests and 128 in flight, a 1 GiB transfer
  through ssh takes 0.84x (download) and 0.91x (upload) of `sftp`, and a 10k-entry listing
  at a 30 ms round trip takes 1.025x the time of its round trips. Tree transfers of small
  files take three round trips per file and run at 0.87x to 0.97x of `sftp -rp`.

## Consequences

- The protocol is path-based and has no inode numbers. Symlink safety is weaker than on a
  local disk (R-3), and a move across hosts cannot identify the remote file
  ([ADR 0024](0024-upload-commit-and-best-effort-cross-host-moves.md)).
- ssh may prompt, and needs the terminal to do so
  ([ADR 0023](0023-ssh-prompts-through-the-terminal-hand-off.md)).
- Features depend on what the server announces: `hardlink`, `posix-rename`, `fsync`,
  `statvfs`, `limits`, `home-directory`. A server that is not OpenSSH, announcing only
  `hardlink`, `posix-rename` and `statvfs`, passed the manual server check (A-SF-12).
- OpenSSH's `sftp-server` serves one request at a time, so a request that meets a FIFO can
  hold a session. A session that stays silent for 2 s after a cancel is ended.

## Alternatives considered

| Alternative | Why not |
|---|---|
| `russh` and `russh-sftp` | tokio, about 120 more crates, a C crypto backend, a config parser without `Include` or `Match`, `known_hosts` without wildcards, `@revoked` or `@cert-authority`, and RUSTSEC-2023-0071, which forces RSA keys off |
| `openssh` and `openssh-sftp-client` | The same transport, but async, with tokio and a multiplex master. It remains the fallback if the own client ever proves too costly to maintain |
| `ssh2` (libssh2) | C with OpenSSL, no `ssh_config`, no `ProxyJump` |
| `sshfs` or `rclone mount` as the feature | A dead link leaves syscalls stuck, no inotify, and `RENAME_NOREPLACE` and identities depend on FUSE |

## References

- [TDR-3 5](../tdr/0003-archives-quick-view-and-sftp.md#5-sftp), R-6 in
  [TDR-3 1.3](../tdr/0003-archives-quick-view-and-sftp.md#13-invariants)
- [TDR-3 appendix A, E-18, E-19, E-30, E-33](../tdr/0003-archives-quick-view-and-sftp.md#appendix-a-implementation-decisions)
