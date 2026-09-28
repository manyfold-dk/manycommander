#![forbid(unsafe_code)]
//! The F1 help overlay: keymap, file-operation semantics, durability contract (design 8,
//! NFR-DUR; phase 2 keys, results tab and copy fidelity: P2 5.4, 9, 10).

pub const TEXT: &[&str] = &[
    "KEYS (command line empty)",
    "  Enter            enter directory / open file with xdg-open",
    "  Backspace        parent directory          Alt+Up    parent directory",
    "  Home / End       first / last entry        Ctrl+A     mark all",
    "  Ctrl+U           swap panels               Space      mark (directory: size)",
    "  Ctrl+D           go to directory           Ctrl+F     quick filter",
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
    "  Ctrl+F3..F6      sort by name, extension, size, time (again: reverse)",
    "  F3 view  F4 edit  Shift+F4 new file  F5 copy  F6 move  Shift+F6 rename",
    "  F7 mkdir  F8 trash  Shift+F8 delete permanently  F10, Alt+X quit",
    "  Alt+F7 find files  Shift+F2 compare directories  Alt+L links  Alt+A attributes",
    "  Ctrl+T new tab  Ctrl+W close tab  Alt+PgUp/PgDn previous/next tab  Ctrl+1..9 tab",
    "COMMAND LINE (text typed)",
    "  Enter runs the line with $SHELL -c in the panel's directory; cd <dir> changes",
    "  the panel; z <keywords> goes to the best frequent directory (z alone: Ctrl+D).",
    "  Ctrl+A/E start/end, Ctrl+U/K kill to start/end, Ctrl+W kill word,",
    "  Ctrl+P/N history, Esc clears. Ctrl+D, Ctrl+F and Ctrl+M do nothing here.",
    "",
    "GO TO DIRECTORY (Ctrl+D)",
    "  Type keywords to filter: bookmarks (*) first, then frequent directories by",
    "  frecency (zoxide's ranking included when installed). Enter go, Insert bookmark",
    "  this directory, Delete remove the bookmark / forget the directory, Esc close.",
    "QUICK FILTER (Ctrl+F)",
    "  Part of the name or a glob (* ? [...]) over the whole name, ignoring ASCII case.",
    "  Enter or Ctrl+F keep the filter, Esc clears it; a new directory clears it.",
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
    "",
    "DURABILITY",
    "  Copy does not fsync, like cp. Move keeps every file in at least one complete,",
    "  committed place at every moment, also across a crash on filesystems that",
    "  honour syncfs (btrfs, ext4, xfs). A crash can leave the last batch in both",
    "  places, never in neither. After a crash, .mc-partial-* files may remain; they",
    "  are never deleted automatically.",
];

#[cfg(test)]
mod tests {
    use super::TEXT;
    use unicode_width::UnicodeWidthStr;

    /// Every line fits the overlay of an 88-column terminal without being cut: the
    /// overlay leaves one column on each side, and its frame takes one more.
    #[test]
    fn lines_fit_an_88_column_terminal() {
        for l in TEXT {
            assert!(l.width() <= 84, "{} columns: {l}", l.width());
        }
    }
}
