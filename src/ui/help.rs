#![forbid(unsafe_code)]
//! The F1 help overlay: keymap, file-operation semantics, durability contract (design 8,
//! NFR-DUR; phase 2 keys, results tab and copy fidelity: P2 5.4, 9, 10; phase 3 archives,
//! quick view and SFTP, their keys and their weaker guarantees: P3 3, 4, 5, 6, R-1 to R-5,
//! NFR-DUR).

/// The overlay's frame title: the program and the version that runs, so the top of the help
/// says which release it describes, whatever line it is scrolled to.
pub fn title() -> String {
    format!("manycommander {} -- Help", env!("CARGO_PKG_VERSION"))
}

pub const TEXT: &[&str] = &[
    "TYPING filters the active panel (QUICK FILTER below). Ctrl+E: to the command line.",
    "KEYS (the command line without the focus)",
    "  Enter            enter directory or archive / open file with xdg-open",
    "  Backspace        parent directory          Alt+Up    parent directory",
    "  Home / End       first / last entry        Ctrl+A     mark all",
    "  Ctrl+U           swap panels               Space      mark (directory: size)",
    "  Ctrl+D           go to directory           Ctrl+F     edit the quick filter",
    "  Ctrl+E           command line              Alt+O      open file as an archive",
    "  Ctrl+W           close tab",
    "  Ctrl+M           multi-rename (needs the kitty keyboard protocol)",
    "  Esc              stop loading, a search or a compare; ask to cancel a job",
    "KEYS (always)",
    "  Tab              other panel               Up/Down/PgUp/PgDn  cursor",
    "  Insert           mark and move down",
    "  Alt+= / Alt+- / Alt+*   mark by glob / unmark by glob / invert marks",
    "  Ctrl+S           quick search              Alt+.      hidden files",
    "  Ctrl+R           re-read both panels       Alt+Left/Right  history",
    "  Alt+Enter        insert quoted name        Alt+P      insert quoted path",
    "  Ctrl+O           show the last command's output",
    "  Ctrl+Q           quick view on / off       Alt+Q      preview now",
    "  Ctrl+F3..F6      sort by name, extension, size, time (again: reverse)",
    "  F3 view  F4 edit  Shift+F4 new file  F5 copy  F6 move  Shift+F6 rename",
    "  F3 on pictures, documents, audio, video and web pages: their application",
    "  F7 mkdir  F8 trash  Shift+F8 delete permanently  F10, Alt+X quit",
    "  Alt+F7 find files  Shift+F2 compare directories  Alt+L links  Alt+A attributes",
    "  Ctrl+T new tab  Alt+PgUp/PgDn previous/next tab  Ctrl+1..9 tab",
    "COMMAND LINE (Ctrl+E, or text put on it: Alt+Enter, Alt+P, Ctrl+P, a paste)",
    "  Typing goes to the line while it has the focus. Enter runs it with $SHELL -c in",
    "  the panel's directory; cd <dir> changes the panel; z <keywords> goes to the best",
    "  frequent directory (z alone: Ctrl+D). cd sftp://[user@]host[:port][/path]",
    "  connects (SFTP below). Enter and Esc (clears) give the focus back to the panel.",
    "  Ctrl+A/E start/end, Ctrl+U/K kill to start/end, Ctrl+W kill word,",
    "  Ctrl+P/N history. With text on the line, Ctrl+D, Ctrl+F, Ctrl+M and Alt+O do",
    "  nothing; on an empty line they leave it and act, and Backspace just leaves it.",
    "",
    "GO TO DIRECTORY (Ctrl+D)",
    "  Type keywords to filter: bookmarks (*) first, then frequent directories by",
    "  frecency (zoxide's ranking included when installed). Enter go, Insert bookmark",
    "  this directory, Delete remove the bookmark / forget the directory, Esc close.",
    "QUICK FILTER (type a letter, or Ctrl+F to edit the filter)",
    "  Part of the name ignoring case, or a glob (* ? [...]) over the whole name. When",
    "  no name contains the text, the closest names show (\"fuzzy\"): from four letters",
    "  one wrong, missing or swapped letter, from six also an extra one, from nine two",
    "  such typos. The cursor goes to a name that starts with the text. Enter keeps",
    "  the filter and opens the entry; Ctrl+F keeps it; Esc clears it; Backspace on an",
    "  empty line closes it. A typed letter starts a new filter; a new directory",
    "  clears it.",
    "FIND FILES (Alt+F7) AND THE RESULTS TAB",
    "  Results fill a new tab. Enter in a results tab goes to the file (a directory:",
    "  into it) and Alt+Left returns; F3/F4 open it. Backspace goes to the search root.",
    "  F5 F6 F8 Shift+F8 Shift+F6 Alt+L Alt+A Ctrl+M act on the results; Ctrl+R",
    "  re-reads them; Esc stops the search. Content search does not read the holes",
    "  of sparse files: text that needs their NUL bytes is not found there.",
    "MULTI-RENAME (Ctrl+M)",
    "  Masks: [N] name, [E] extension, [N2-5] characters 2 to 5, [C] counter,",
    "  [P] parent directory, [Y] [M] [D] [h] [m] [s] modification time. The preview",
    "  shows every new name (PgUp/PgDn scroll); Enter renames only when no row has an",
    "  error. It never overwrites; swaps and cycles work. In the dialog, Ctrl+Z undoes",
    "  the last multi-rename of this session.",
    "COMPARE (Shift+F2), LINKS (Alt+L), ATTRIBUTES (Alt+A)",
    "  Compare marks what differs: by date and size, or by content.",
    "  Links are symbolic (relative or absolute) or hard, and never replace an entry.",
    "  Mode: octal (644) or clauses (u+x,g-w,o=r); an empty field leaves it unchanged.",
    "",
    "ARCHIVES (read-only)",
    "  Enter opens zip, jar, apk, whl, tar, tar.gz, tgz, tar.zst, tzst, tar.xz, txz,",
    "  tar.bz2, tbz2, tbz and 7z by name; Alt+O tries any file. .. at the root leaves.",
    "  Enter, F3 and F4 on a member open a copy; an edited copy is kept, never written",
    "  back. F5 extracts into the other panel; nothing writes into an archive. A file",
    "  that fails the format check stays a file: Ctrl+E, then xdg-open and Alt+Enter",
    "  on the command line open it. Tar and solid 7z archives extract in one pass: Retry",
    "  on a partly read member fails. 7z members packed with zstd or PPMd fail as",
    "  unsupported, AES ones as encrypted.",
    "QUICK VIEW (Ctrl+Q)",
    "  The inactive side shows the entry under the cursor: a picture (JPEG, PNG, GIF,",
    "  WebP, BMP) through kitty graphics, sixel or halfblocks, else an info card. Tab",
    "  swaps sides; F5 and F6 still go to the hidden panel. Remote files and members",
    "  of compressed tars load only on Alt+Q. Pictures need truecolor. Inside tmux,",
    "  kitty graphics need tmux's allow-passthrough option, which manycommander never",
    "  sets; without it, halfblocks (sixel in tmux only by setting it). [preview]",
    "  protocol in config.toml:",
    "  \"auto\" (the terminal probe), \"kitty\", \"sixel\", \"halfblocks\" or \"off\".",
    "SFTP (cd sftp://..., or a bookmark)",
    "  The connect hands the terminal to your ssh, which asks for passphrases,",
    "  passwords and host keys itself; Ctrl+C cancels. With ProxyJump or ProxyCommand,",
    "  Ctrl+Z at a prompt can stop only the proxy: press Ctrl+C. An ssh that wants the",
    "  terminal later (ControlMaster ask) stops in the background: the operation",
    "  waits, and Esc ends the session. Ctrl+R reconnects a lost session.",
    "  F5 and F6 upload and download, F7 mkdir, Shift+F6 rename, Shift+F8 delete; F4",
    "  asks to upload an edited copy. No remote trash: F8 is refused.",
    "  [sftp] ssh = [\"ssh\", ...] in config.toml: manycommander's fixed options follow",
    "  the program and win; -o, -A, -X, -Y, -t and -e are rejected.",
    "SFTP GUARANTEES (weaker than on a local disk)",
    "  An upload is written to .<name>.mc-partial-<random> and linked into place with",
    "  hardlink@openssh.com, which never replaces. A server without it gets the file",
    "  under its final name while it is written. After a lost session, the report",
    "  names the file that may be partial. Overwrite needs posix-rename@openssh.com,",
    "  else \"the server cannot replace a file atomically\". SFTP works on paths: a",
    "  name swapped between a check and a request goes undetected, and a swap to a",
    "  FIFO blocks the server (Esc; 2 s later manycommander ends the session). Rename",
    "  checks the new name first; a name made in between can be lost on servers",
    "  other than OpenSSH, whose rename refuses an existing file, directory or",
    "  symlink. Symlinks are copied as links and never descended.",
    "",
    "FILE OPERATIONS",
    "  Verbs act on the visible marked entries; with none, on the entry under the cursor.",
    "  Marks hidden by the filter or the hidden toggle are kept but not acted on.",
    "  Copy never shows a partial file: data goes to .<name>.mc-partial-<random>",
    "  and is renamed into place. An existing file is replaced only after you answer",
    "  Overwrite; the old file is never truncated. Symbolic links are copied as links.",
    "  Copy and move keep sparse files sparse and keep hard links between the copied",
    "  files; a file also linked from outside the selection becomes a separate file.",
    "  Not preserved: ownership, ACLs, xattrs.",
    "  Move renames when it can. Across filesystems it copies, then deletes the source",
    "  only after the copy is durable (syncfs); a changed source is kept. On vfat and",
    "  exfat the change check is best-effort. Mount points are never copy-deleted.",
    "  Trash follows the freedesktop.org specification; it never copies across",
    "  filesystems and never deletes. Shift+F8 deletes only after you type delete.",
    "  Extraction: no special files, no setuid, setgid or sticky bits, no member",
    "  outside the destination, and never more bytes than a member declares.",
    "",
    "DURABILITY",
    "  Copy does not fsync, like cp. Move keeps every file in at least one complete,",
    "  committed place at every moment, also across a crash on filesystems that",
    "  honour syncfs (btrfs, ext4, xfs). A crash can leave the last batch in both",
    "  places, never in neither. After a crash, .mc-partial-* files may remain; they",
    "  are never deleted automatically.",
    "  Extraction and downloads do not fsync either. A move across hosts is only",
    "  best-effort: an upload move deletes each local source after its upload is",
    "  committed, synced on the server only with fsync@openssh.com (else the report",
    "  says \"not synced on the server\"); a download move syncs here and keeps the",
    "  remote sources, because SFTP cannot identify them.",
];

#[cfg(test)]
mod tests {
    use super::{TEXT, title};
    use unicode_width::UnicodeWidthStr;

    /// Every line fits the overlay of an 88-column terminal without being cut: the
    /// overlay leaves one column on each side, and its frame takes one more.
    #[test]
    fn lines_fit_an_88_column_terminal() {
        for l in TEXT {
            assert!(l.width() <= 84, "{} columns: {l}", l.width());
        }
    }

    #[test]
    fn title_names_the_running_version() {
        let t = title();
        assert!(t.starts_with("manycommander "), "{t}");
        assert!(t.contains(env!("CARGO_PKG_VERSION")), "{t}");
    }
}
