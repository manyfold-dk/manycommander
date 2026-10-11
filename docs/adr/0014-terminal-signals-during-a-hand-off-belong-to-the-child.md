# ADR 0014: Terminal signals during a hand-off belong to the child

## Status

Accepted -- 2026-09-27; amended 2026-10-10.

The amendment: a signal the kernel sent is the child's whenever the signal thread reads it,
not only while a child runs.

## Context

F3, F4 and the command line hand the terminal to a child program: manycommander leaves raw
mode and the alternate screen, runs the child with inherited stdio in manycommander's own
process group, and restores the terminal afterwards (TDR-1 6). While the child runs, the terminal
is in cooked mode, so `Ctrl+C`, `Ctrl+\` and `Ctrl+Z` make the kernel send `SIGINT`,
`SIGQUIT` and `SIGTSTP` to the foreground process group: the child **and** manycommander.

manycommander also handles those signals for itself. `SIGINT` (with `SIGTERM` and
`SIGHUP`) quits through the terminal-restoring path, and `SIGTSTP` suspends. All signals go
through one signal thread: the handler only writes to a self-pipe, and the thread turns
each signal into an event (TDR-1 3.1).

The first rule, from 2026-09-27, ignored `SIGINT`, `SIGQUIT` and `SIGTSTP` while a flag said
a child was running. That rule had a race. A pager that ends at once on `Ctrl+C` lets the
UI thread clear the flag before the signal thread reads the `SIGINT`. The thread then saw no
child, and manycommander quit. An SFTP pty test (A-SF-6) started failing in CI on
2026-10-10, including on a re-run of a commit that had passed. A 50 ms delay in the signal
thread made it fail every time locally.

## Decision

The signal thread reads each signal's origin (`signal-hook`'s `extended-siginfo`), and
drops `SIGINT`, `SIGQUIT` and `SIGTSTP` when either holds:

- the kernel sent it (`SI_KERNEL`), whenever the thread reads it; or
- a child runs, whoever sent it.

The terminal sends these signals only outside raw mode, that is during a hand-off, and the
kernel is then the sender. So a kernel-sent one belongs to the child even when the thread
reads it after the child has ended. The same signals sent with `kill` while no child runs
still quit or stop manycommander.

## Rationale

- The origin of a signal is a property of the signal; whether a child runs is a property of
  the moment the thread happens to read it. Deciding by the origin removes the race instead
  of narrowing it.
- In raw mode the terminal sends keys, not signals, so a kernel-sent `SIGINT` cannot come
  from a key meant for manycommander.
- `kill -INT` and `kill -TSTP` from another terminal keep their meaning, which scripts and
  the A-UI-3 checks rely on.
- `extended-siginfo` needs a small C file that `cc` builds, and `cc` was already a build
  dependency.

## Consequences

- With the fix, the 50 ms delay that made the race certain leaves A-SF-6 passing. Under
  tmux, a real `Ctrl+C` in a pager logs "signal dropped: the child's" with
  `cause=Kernel`.
- ssh runs in its own process group (ADR 0023), so these keys never reach an open SFTP
  session; during a connect, ssh owns the terminal and gets them alone.
- `SIGKILL` of manycommander itself still cannot be handled; `SIGTERM`, `SIGHUP` and a
  `kill`-sent `SIGINT` quit through the restore path.

## Alternatives considered

| Alternative | Why not |
|---|---|
| Keep the "child running" flag and clear it later | Any delay only narrows the race; a slow signal thread still loses |
| Run the child in its own process group and hand it the terminal | The terminal's job control would then also have to be handed back and forth for every F3, F4 and command; the program would differ from how a shell runs it |
| Ignore `SIGINT` in manycommander altogether | `kill -INT` from outside would no longer quit it through the restore path |

## References

- [TDR-1 3.1 Threads and events](../tdr/0001-core.md#31-threads-and-events)
- [TDR-1 6 Command line and hand-off](../tdr/0001-core.md#6-command-line-and-hand-off)
- [ADR 0023: SSH prompts go through the terminal hand-off](0023-ssh-prompts-through-the-terminal-hand-off.md)
