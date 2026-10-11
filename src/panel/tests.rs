#![forbid(unsafe_code)]
//! Panel tests that span its modules: loads and their history, marks, the sort, the filter
//! and the cursor, on directory, results, archive and remote panels (TDR-1 5, TDR-2 2.4,
//! TDR-2 4, TDR-3 2.2).

use super::*;
use crate::archive::ArchiveIndex;
use crate::provider::{StatKey, Target};
use crate::remote::RemoteProvider;
use crate::remote::url::RemoteDir;

#[test]
fn relative_names_split_at_the_last_slash() {
    assert_eq!(split_rel(b"a/b/c.txt"), (&b"a/b"[..], &b"c.txt"[..]));
    assert_eq!(split_rel(b"c"), (&b""[..], &b"c"[..]));
}

/// A directory panel of `names` (files, `d...` directories) listed in `/p`.
fn listed(names: &[&[u8]]) -> Panel {
    let mut p = Panel::new(0, "/p".into());
    let req = p.navigate("/p".into(), None, Alive::running());
    let mut arena = Vec::new();
    let es = names
        .iter()
        .enumerate()
        .map(|(i, n)| {
            let kind = if n.starts_with(b"d") {
                crate::fsops::sys::Kind::Dir
            } else {
                crate::fsops::sys::Kind::File
            };
            let m = crate::fsops::sys::Meta {
                kind,
                size: 10 * (names.len() - i) as u64,
                ..Default::default()
            };
            Entry::new(&mut arena, n, &m)
        })
        .collect();
    p.on_batch(req.generation, es, &arena);
    p.on_done(req.generation, "/p".into());
    p
}

/// Declared sizes (an archive's, a server's) can add up past `u64::MAX`: the marked bytes
/// saturate, and an unmark after that recounts instead of subtracting.
#[test]
fn marked_bytes_saturate_and_recount() {
    let mut p = listed(&[b"da", b"db", b"f"]);
    p.on_dir_size(OsStr::new("da"), Some(u64::MAX - 1));
    p.on_dir_size(OsStr::new("db"), Some(u64::MAX - 1));
    let idx = |p: &Panel, n: &[u8]| p.list.find(n).unwrap();
    let (a, b, f) = (idx(&p, b"da"), idx(&p, b"db"), idx(&p, b"f"));
    p.set_mark(a, true);
    p.set_mark(b, true);
    p.set_mark(f, true);
    assert_eq!(p.marked_bytes, u64::MAX);
    p.set_mark(b, false);
    assert_eq!(
        p.marked_bytes,
        u64::MAX,
        "da and f still add up past the maximum"
    );
    p.set_mark(a, false);
    assert_eq!(p.marked_bytes, 10);
}

/// A failed or cancelled navigation leaves the history as it was: no second entry of
/// the place the panel returns to, and the forward stack kept. A completed one records
/// the place it left, once (TDR-2 2.4).
#[test]
fn only_a_completed_navigation_records_the_place_it_left() {
    let mut p = listed(&[b"a"]);
    p.history.forward.push(Place::Dir("/next".into()));
    let req = p.navigate("/p/denied".into(), None, Alive::running());
    p.on_failed(req.generation, "Permission denied".into());
    assert_eq!(p.dir, Path::new("/p"));
    p.navigate("/p/slow".into(), None, Alive::running());
    p.cancel_load();
    assert!(p.history.back.is_empty(), "nothing to go back to");
    assert!(
        matches!(p.history.forward.last(), Some(Place::Dir(d)) if d == Path::new("/next")),
        "the forward stack stays"
    );
    let req = p.navigate("/p/ok".into(), None, Alive::running());
    assert!(p.history.back.is_empty(), "not before the load completes");
    p.on_done(req.generation, "/p/ok".into());
    assert!(matches!(&p.history.back[..], [Place::Dir(d)] if d == Path::new("/p")));
    assert!(
        p.history.forward.is_empty(),
        "a new place clears the forward stack"
    );
}

/// `Esc` returns to the listing left behind, sorted by the order chosen during the load.
#[test]
fn esc_sorts_the_place_it_returns_to_by_the_order_in_use() {
    let mut p = listed(&[b"a", b"b", b"c"]);
    assert_eq!(p.current_name(), None, "the cursor is on `..`");
    p.cursor_to_name(b"b");
    p.navigate("/p/slow".into(), None, Alive::running());
    p.set_sort(SortKey::Size);
    p.cancel_load();
    let order: Vec<&[u8]> = p.list.visible.iter().map(|&i| p.list.name(i)).collect();
    // Sizes 30, 20, 10: smallest first.
    assert_eq!(order, [&b"c"[..], b"b", b"a"]);
    assert_eq!(
        p.current_name(),
        Some(&b"b"[..]),
        "the cursor keeps its name"
    );
}

/// A refresh's fresh entries know neither a computed directory size nor a symlink's
/// target until its second pass: both are carried over by name, so a sized directory
/// keeps its size and a link to a directory stays with the directories.
#[test]
fn a_refresh_keeps_computed_sizes_and_known_link_targets() {
    use crate::fsops::sys::{Kind, Meta};
    let m = |kind, size| Meta {
        kind,
        size,
        ..Meta::default()
    };
    let mut p = Panel::new(0, "/p".into());
    let list = |p: &mut Panel, refresh: bool| {
        let mut arena = Vec::new();
        let es = vec![
            Entry::new(&mut arena, b"dir", &m(Kind::Dir, 4096)),
            Entry::new(&mut arena, b"link", &m(Kind::Symlink, 3)),
            Entry::new(&mut arena, b"file", &m(Kind::File, 1)),
        ];
        if refresh {
            let req = p.refresh(Alive::running());
            let l = Listing::sorted(es, arena, p.sort);
            p.on_listing(req.generation, l);
            p.on_done(req.generation, "/p".into());
        } else {
            let req = p.navigate("/p".into(), None, Alive::running());
            p.on_batch(req.generation, es, &arena);
            p.on_done(req.generation, "/p".into());
            p.on_links(req.generation, &[(1, LinkKind::Dir)]);
            p.ensure_sorted();
        }
    };
    list(&mut p, false);
    p.on_dir_size(OsStr::new("dir"), Some(12_345));
    let rows = |p: &Panel| -> Vec<Vec<u8>> {
        p.list
            .visible
            .iter()
            .map(|&i| p.list.name(i).to_vec())
            .collect()
    };
    assert_eq!(rows(&p), [&b"dir"[..], b"link", b"file"]);
    let generation = p.generation;
    list(&mut p, true);
    let i = p.list.find(b"dir").unwrap();
    let e = p.list.entries[i as usize];
    assert_eq!((e.size, e.flags & SIZED != 0), (12_345, true));
    let link = p.list.entries[p.list.find(b"link").unwrap() as usize];
    assert_eq!(link.link, LinkKind::Dir);
    assert_eq!(rows(&p), [&b"dir"[..], b"link", b"file"], "no flicker");
    // A size asked for before the refresh still applies; one from before the
    // navigation that brought the panel here does not.
    assert!(p.accepts_size(generation) && p.accepts_size(p.generation));
    assert!(!p.accepts_size(generation - 1));
    p.navigate("/q".into(), None, Alive::running());
    assert!(!p.accepts_size(generation));
}

/// A hidden tab loses its watch: the refresh that shows it again reports its directory,
/// so the app watches it again.
#[test]
fn a_tab_shown_again_reports_its_directory_to_watch() {
    let mut p = listed(&[b"a"]);
    let req = p.refresh(Alive::running());
    assert_eq!(
        p.on_done(req.generation, "/p".into()),
        None,
        "watched already"
    );
    p.release();
    let req = p.refresh(Alive::running());
    assert_eq!(
        p.on_done(req.generation, "/p".into()),
        Some(PathBuf::from("/p"))
    );
}

fn search(id: u64) -> Arc<Search> {
    Arc::new(Search::new(
        id,
        crate::find::FindSpec {
            root: "/r".into(),
            ..Default::default()
        },
    ))
}

fn stashed(id: u64) -> Stashed {
    Stashed {
        search: search(id),
        list: Listing::default(),
        cursor: None,
        filter: Filter::default(),
    }
}

#[test]
fn history_keeps_three_results_places() {
    let mut h = History::default();
    h.push(Place::Dir("/a".into()));
    for id in 1..=5 {
        h.push(Place::Results(Box::new(stashed(id))));
        h.push(Place::Dir(format!("/d{id}").into()));
    }
    assert_eq!(h.results_places(), History::RESULTS);
    let ids: Vec<u64> = h
        .back
        .iter()
        .filter_map(|p| match p {
            Place::Results(s) => Some(s.search.id),
            _ => None,
        })
        .collect();
    assert_eq!(ids, [3, 4, 5], "the oldest go first");
    assert_eq!(h.back.len(), 1 + 3 + 5, "directories stay");
    // Forward places count too.
    h.forward.push(Place::Results(Box::new(stashed(6))));
    h.trim();
    assert_eq!(h.results_places(), History::RESULTS);
}

#[test]
fn a_results_panel_has_no_parent_row_and_is_not_a_directory() {
    let mut p = Panel::results(3, search(1));
    assert!(!p.is_directory() && !p.has_parent());
    assert_eq!(p.dir, Path::new("/r"));
    assert!(p.searching());
    let m = crate::fsops::sys::Meta {
        kind: crate::fsops::sys::Kind::File,
        ..Default::default()
    };
    let mut names = Vec::new();
    let es = vec![
        Entry::new(&mut names, b"x/b", &m),
        Entry::new(&mut names, b"a", &m),
    ];
    p.append_results(es, &names);
    p.force_sort();
    assert_eq!(p.rows(), 2);
    assert_eq!(p.row(0), Some(Row::Entry(1)));
    assert_eq!(p.current_name(), Some(&b"a"[..]));
}

/// A results panel holding `names`, sorted.
fn results_of(names: &[&[u8]]) -> Panel {
    let mut p = Panel::results(3, search(1));
    let m = crate::fsops::sys::Meta {
        kind: crate::fsops::sys::Kind::File,
        ..Default::default()
    };
    let mut arena = Vec::new();
    let es = names
        .iter()
        .map(|n| Entry::new(&mut arena, n, &m))
        .collect();
    p.append_results(es, &arena);
    p.force_sort();
    p
}

fn shown(p: &Panel) -> Vec<String> {
    let mut v: Vec<String> = p
        .list
        .visible
        .iter()
        .map(|&i| String::from_utf8_lossy(p.list.name(i)).into_owned())
        .collect();
    v.sort();
    v
}

/// The fuzzy tier (TDR-2 4) shows only the fewest edits, and only when no name contains
/// the text; it waits for entries not yet sorted in.
#[test]
fn the_fuzzy_tier_shows_the_closest_names() {
    let mut p = results_of(&[b"prezentatoin.odp", b"presentatoin.odp", b"notes.txt"]);
    p.set_filter(b"presentation");
    assert_eq!(shown(&p), ["presentatoin.odp"]);
    assert_eq!(p.list.fuzzy, 1);
    let mut p = results_of(&[b"prezentatoin.odp", b"notes.txt"]);
    p.set_filter(b"presentation");
    assert_eq!(shown(&p), ["prezentatoin.odp"]);
    assert_eq!(p.list.fuzzy, 2);
    // An exact match wins; the fuzzy tier is gone.
    let m = crate::fsops::sys::Meta {
        kind: crate::fsops::sys::Kind::File,
        ..Default::default()
    };
    let mut arena = Vec::new();
    let e = Entry::new(&mut arena, b"final-presentation.pdf", &m);
    p.append_results(vec![e], &arena);
    // Not sorted in yet: no fuzzy tier over a partial order.
    p.set_filter(b"presentatio");
    assert!(shown(&p).is_empty());
    assert_eq!(p.list.fuzzy, 0);
    p.force_sort();
    assert_eq!(shown(&p), ["final-presentation.pdf"]);
    assert_eq!(p.list.fuzzy, 0);
    // Three letters get no fuzzy tier.
    p.set_filter(b"xpd");
    assert!(shown(&p).is_empty());
    // A name that only folding matches (the Kelvin sign is a `k`) comes from the fuzzy
    // tier, and says so.
    let mut p = results_of(&["\u{212a}eys.txt".as_bytes(), b"notes.txt"]);
    p.set_filter(b"keys");
    assert_eq!(shown(&p), ["\u{212a}eys.txt"]);
    assert_eq!(p.list.fuzzy, 1);
}

/// A typed filter puts the cursor on the first name that starts with it; in a results
/// tab, the name is the last component of the path.
#[test]
fn the_cursor_goes_to_a_name_that_starts_with_the_filter() {
    let mut p = results_of(&[b"abc/xreadme", b"zzz/README.md", b"readme-dir/notes"]);
    p.set_filter(b"readme");
    assert_eq!(
        shown(&p),
        ["abc/xreadme", "readme-dir/notes", "zzz/README.md"]
    );
    assert_eq!(p.current_name(), Some(&b"zzz/README.md"[..]));
}

#[test]
fn leaving_a_results_place_stashes_it_when_the_load_completes() {
    let mut p = Panel::results(3, search(9));
    let m = crate::fsops::sys::Meta::default();
    let mut names = Vec::new();
    p.append_results(vec![Entry::new(&mut names, b"d/f", &m)], &names);
    p.force_sort();
    let req = p.navigate_full(
        "/r/d".into(),
        Some(b"f".to_vec()),
        Alive::running(),
        false,
        Record::New,
    );
    assert!(p.is_directory());
    assert_eq!(
        p.history.results_places(),
        0,
        "not before the load completes"
    );
    // Esc returns to the results.
    p.cancel_load();
    assert!(p.search().is_some_and(|s| s.id == 9));
    assert_eq!(p.list.entries.len(), 1);
    let req2 = p.navigate_full("/r/d".into(), None, Alive::running(), false, Record::New);
    assert!(req2.generation > req.generation);
    p.on_done(req2.generation, "/r/d".into());
    assert_eq!(p.history.results_places(), 1);
    let Some(Place::Results(s)) = p.history_back() else {
        panic!("a results place");
    };
    assert!(p.show_results(*s, Record::Back).is_none());
    assert_eq!(p.search().map(|s| s.id), Some(9));
    assert_eq!(p.current_name(), Some(&b"d/f"[..]));
    assert!(matches!(p.history_forward(), Some(Place::Dir(d)) if d == Path::new("/r/d")));
    // One results place replacing another keeps the first one's cursor.
    let mut names = Vec::new();
    p.append_results(vec![Entry::new(&mut names, b"a", &m)], &names);
    p.force_sort();
    p.cursor_to_name(b"d/f");
    p.show_results(stashed(10), Record::Forward);
    assert_eq!(p.search().map(|s| s.id), Some(10));
    let Some(Place::Results(s)) = p.history_back() else {
        panic!("the first results place");
    };
    assert_eq!(s.cursor.as_deref(), Some(&b"d/f"[..]));
}

fn key() -> StatKey {
    StatKey {
        dev: 1,
        ino: 2,
        size: 3,
        mtime: crate::fsops::sys::Ts { sec: 4, nsec: 5 },
        ctime: crate::fsops::sys::Ts { sec: 6, nsec: 7 },
    }
}

fn in_archive(p: &mut Panel, index: &Arc<ArchiveIndex>, inner: &[u8]) {
    p.source = Source::Archive(ArchiveView {
        index: index.clone(),
        archive: "/x/a.zip".into(),
        key: key(),
        inner: VPath::parse(inner).unwrap(),
    });
    p.loaded_once = true;
}

fn target() -> Target {
    Target {
        user: Some("u".into()),
        host: "h".into(),
        port: None,
    }
}

/// TDR-3 2.2: leaving an archive puts what reopens it into the history, never the index; a
/// history place of an archive or a server is not repeated; `..` leaves either.
#[test]
fn archive_places_name_what_to_reopen() {
    let index = Arc::new(ArchiveIndex::detached("/x/a.zip".into(), key()));
    let mut p = Panel::new(0, "/x".into());
    in_archive(&mut p, &index, b"d/e");
    assert!(p.has_parent(), "`..` leaves the archive");
    assert!(!p.is_directory() && !p.is_results() && p.search().is_none());
    assert_eq!(
        p.archive().map(|v| v.inner.to_bytes()),
        Some(b"/d/e".to_vec())
    );
    p.filter = Filter::new(b"q");
    // Leaving for the archive's own directory is a new place, and drops the filter.
    let req = p.navigate_full(
        "/x".into(),
        Some(b"a.zip".to_vec()),
        Alive::running(),
        false,
        Record::New,
    );
    assert!(p.is_directory() && p.filter.is_empty());
    p.on_done(req.generation, "/x".into());
    assert_eq!(Arc::strong_count(&index), 1, "the history pins no index");
    let Some(Place::Archive {
        archive,
        key: k,
        inner,
    }) = p.history_back()
    else {
        panic!("an archive place");
    };
    assert_eq!(
        (archive, k, inner.to_bytes()),
        (PathBuf::from("/x/a.zip"), key(), b"/d/e".to_vec())
    );

    // The same place twice in a row is kept once.
    for _ in 0..2 {
        in_archive(&mut p, &index, b"d");
        let req = p.navigate_full("/y".into(), None, Alive::running(), false, Record::New);
        p.on_done(req.generation, "/y".into());
    }
    let req = p.navigate_full("/z".into(), None, Alive::running(), false, Record::New);
    p.on_done(req.generation, "/z".into());
    in_archive(&mut p, &index, b"d");
    let req = p.navigate_full("/x".into(), None, Alive::running(), false, Record::New);
    p.on_done(req.generation, "/x".into());
    let places: Vec<bool> = p
        .history
        .back
        .iter()
        .map(|pl| matches!(pl, Place::Archive { .. }))
        .collect();
    assert_eq!(places, [true, false, true], "{places:?}");

    // History back from an archive puts it on the forward stack, once the load
    // completes.
    in_archive(&mut p, &index, b"");
    let req = p.navigate_full("/x".into(), None, Alive::running(), false, Record::Back);
    assert!(
        p.history.forward.is_empty(),
        "not before the load completes"
    );
    p.on_done(req.generation, "/x".into());
    assert!(matches!(p.history_forward(), Some(Place::Archive { inner, .. }) if inner.is_root()));
    // A results place shown over an archive stashes the archive's place.
    in_archive(&mut p, &index, b"z");
    p.show_results(stashed(1), Record::Forward);
    assert!(matches!(p.history_back(), Some(Place::Archive { .. })));
    assert_eq!(Arc::strong_count(&index), 1, "nothing holds the index now");
}

/// TDR-3 2.2, 5.7: a remote place keeps the server as typed and the directory, never the
/// session.
#[test]
fn remote_places_name_what_to_reconnect() {
    let session = Arc::new(RemoteProvider::new(
        crate::remote::Session::detached(),
        target(),
    ));
    let mut p = Panel::new(0, "/home/u".into());
    p.source = Source::Remote(RemoteView {
        session: session.clone(),
        target: target(),
        dir: VPath::parse(b"/srv/www").unwrap(),
        home: false,
    });
    p.loaded_once = true;
    assert!(p.has_parent() && p.remote().is_some());
    let req = p.navigate_full("/home/u".into(), None, Alive::running(), false, Record::New);
    p.on_done(req.generation, "/home/u".into());
    assert_eq!(
        Arc::strong_count(&session),
        1,
        "the history pins no session"
    );
    let Some(Place::Remote {
        target: t,
        dir,
        home,
    }) = p.history_back()
    else {
        panic!("a remote place");
    };
    assert_eq!(
        (t, dir.to_bytes(), home),
        (target(), b"/srv/www".to_vec(), false)
    );
    assert!(
        !Place::Remote {
            target: target(),
            dir: VPath::root(),
            home: false,
        }
        .same(&Place::Dir("/".into()))
    );
    // Entering a server from its tab's own local directory is a new place: `Alt+Left`
    // returns to the directory. A hidden remote tab keeps the place, not the session.
    let req = p.navigate_remote(
        RemoteView {
            session: session.clone(),
            target: target(),
            dir: VPath::root(),
            home: true,
        },
        None,
        Alive::running(),
        Record::New,
    );
    assert_eq!(req.dir, RemoteDir::Home(VPath::root()));
    assert_eq!(p.location(), b"sftp://u@h/~");
    p.on_located(req.generation, VPath::parse(b"/home/u").unwrap());
    assert_eq!(p.location(), b"sftp://u@h/home/u");
    assert!(
        p.history.back.is_empty(),
        "not before the listing completes"
    );
    // Hiding the tab counts the navigation as done: the place it left is recorded.
    p.release();
    assert!(matches!(p.history.back.last(), Some(Place::Dir(d)) if d == Path::new("/home/u")));
    assert!(req.cancel.load(Ordering::SeqCst), "the listing stops");
    assert_eq!(
        Arc::strong_count(&session),
        2,
        "the request holds it, the tab does not"
    );
    drop(req);
    assert!(matches!(
        &p.reopen,
        Some(Place::Remote { dir, home: false, .. }) if dir.to_bytes() == b"/home/u"
    ));
    assert!(p.loading.is_none() && p.is_directory());
}

#[test]
fn lexical_join() {
    assert_eq!(
        join_lexical(Path::new("/a/b"), Path::new("../c")),
        Path::new("/a/c")
    );
    assert_eq!(
        join_lexical(Path::new("/a/b"), Path::new("/x/./y")),
        Path::new("/x/y")
    );
    assert_eq!(
        join_lexical(Path::new("/"), Path::new("..")),
        Path::new("/")
    );
}
