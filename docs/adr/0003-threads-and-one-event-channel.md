# ADR 0003: Plain threads and one event channel; the UI thread makes no filesystem calls

## Status

Accepted -- 2026-09-27.

## Context

manycommander must feel instant: a key reaches the screen within 16 ms at the 99th
percentile while a 100k-entry directory lists and while a 10 GiB copy runs (P-1), and an
idle process wakes up never (P-5). The work behind that is a small number of long, blocking
system calls: `getdents64` and `statx` over a directory, `copy_file_range` over a file,
`syncfs` over a filesystem, `poll` on the terminal.

Some of those calls can block for good. A stalled FUSE or network mount leaves a thread in
the kernel, and no runtime, async or not, can cancel a thread that is blocked in a system
call. The design has to say what happens then, not pretend it cannot happen.

## Decision

- Plain `std::thread` threads and one `std::sync::mpsc` channel into the UI thread. No async
  runtime.
- The UI thread owns all application state and makes **no filesystem system calls**.
  Listing, link classification, directory sizes, `statvfs`, theme parsing and every job run
  on other threads and report through the channel
  ([TDR-1 3.1](../tdr/0001-core.md#31-threads-and-events)).
- The event loop blocks on the channel. A tick runs only while a spinner or a progress row
  is visible. The input thread blocks in `poll` on the terminal and a wake pipe, without a
  timeout.
- Cancellation is an `AtomicBool` that workers check between entries and between chunks.
- **Stuck calls are abandoned, not cancelled.** `Esc` returns a panel to its previous
  directory at once; the blocked thread is left behind, and its late result is dropped by
  a generation number. At most four abandoned threads may exist (`MAX_ABANDONED`); a new
  load into a directory whose earlier load is still stuck is refused. A blocked job shows
  "cancel pending -- the filesystem is not responding".
- Worker, listing and helper threads run under `catch_unwind`; a panic fails that job or
  load, and the application stays up.

## Rationale

- An async runtime adds a scheduler, a dependency tree and colouring of every function, and
  it still runs blocking file system calls on a thread pool. It buys nothing for this
  workload.
- One channel gives one ordering of events and one place where state changes, which keeps
  `update(event)` testable without a terminal.
- Keeping filesystem calls off the UI thread is the only way to hold P-1 against a slow
  disk: any call on the UI thread can block it for as long as the kernel likes.
- Measured on the reference machine: key-to-frame p99 1.22 ms idle and 1.32 ms during a
  10 GiB cross-filesystem copy; 60 s idle with no voluntary context switches and no CPU
  ticks (A-P-1, A-P-5, [perf history](../perf/history.md)).

## Consequences

- A thread stuck in the kernel stays until the call returns. The cap of four bounds the
  leak; at the cap, further loads into stuck places are refused with a message.
- Every later subsystem follows the same pattern: find workers, the compare thread, archive
  scans, the preview thread and SFTP session threads ([TDR-2 2.3](../tdr/0002-navigation-and-bulk-tools.md#23-threads-and-events),
  [TDR-3 2.5](../tdr/0003-archives-quick-view-and-sftp.md#25-threads-and-events)), and
  archive scans, view preparations and preview threads count against the same cap.
- The channel is unbounded; producers bound themselves (progress at 15 Hz, listing batches,
  debounced watchers).
- A refresh that re-sorts 100k entries must sort on the listing thread too; doing it on the
  UI thread measured 21-25 ms and missed P-1 (TDR-2 E-42).

## Alternatives considered

| Alternative | Why not |
|---|---|
| tokio or another async runtime | Blocking calls still need threads; adds a runtime and makes cancellation implicit |
| An input thread that polls with a short timeout | Wakes an idle process periodically and fails P-5 |
| Doing small filesystem calls on the UI thread | Any call can block on a stalled mount and freeze the interface |

## References

- [TDR-1 3.1](../tdr/0001-core.md#31-threads-and-events), [TDR-1 13.1](../tdr/0001-core.md#131-performance-and-responsiveness)
- TDR-1 E-20 (input thread without a timeout), TDR-1 E-34 (no cursor-position query)
