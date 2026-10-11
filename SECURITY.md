# Security policy

## Reporting a vulnerability

Report a vulnerability privately through
[GitHub private vulnerability reporting](https://github.com/manyfold-dk/manycommander/security/advisories/new).
Do not open a public issue for it.

Say what you ran, which release you used (`manycommander --version`) or which commit you
built, and what the operation left on disk. Leave out the contents of your files, host names
and user names; describe them instead. A crafted archive, picture or server reply that
reproduces the problem is welcome as an attachment to the private report.

manycommander has one maintainer, who aims to acknowledge a report within a week. A
confirmed vulnerability is fixed in a patch release, and the advisory is published when the
release is out.

## Supported versions

Only the latest release receives fixes. manycommander has no long-term support branches;
update to the latest release before you report.

## Scope

manycommander handles untrusted input in several places. Reports about any of them are in
scope:

| Area | What should hold | Design |
|---|---|---|
| File operations | No lost data on move, no partial destination file, no overwrite without an answer, symlinks never followed (invariants I-1 to I-7) | [TDR-1 4](docs/tdr/0001-core.md#4-file-operation-safety-highest-risk) |
| Archives | No member is written outside the destination, no special files or privilege bits, bounded memory for any archive | [TDR-3 3](docs/tdr/0003-archives-quick-view-and-sftp.md#3-archives) |
| Pictures and previews | Bounded memory and time for any file; nothing a file contains reaches the terminal as a control sequence | [TDR-3 4](docs/tdr/0003-archives-quick-view-and-sftp.md#4-image-quick-view) |
| SFTP | The user's ssh runs unmodified and by argv; nothing a server sends is trusted as a path or a length | [TDR-3 5](docs/tdr/0003-archives-quick-view-and-sftp.md#5-sftp) |
| File names | A name never reaches a shell unquoted and never reaches the terminal unescaped | [TDR-1 3.2](docs/tdr/0001-core.md#32-paths-and-names), [TDR-1 6](docs/tdr/0001-core.md#6-command-line-and-hand-off) |

Out of scope: the behaviour of `ssh`, `gio`, `xdg-open`, `$PAGER` and `$EDITOR`, which
manycommander starts but does not control, and a local attacker who already runs code as
your user.
