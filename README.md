# manycommander

A dual-pane, keyboard-driven terminal file manager for [Omarchy](https://omarchy.org/), in
the Total Commander tradition: two panels, the F3-F8 verbs, a command line and tabs. It
takes its colours from the active Omarchy theme and re-themes itself live when the theme
changes.

**Documentation: [manycommander.app](https://manycommander.app)** -- install, launch, the
keymap, configuration, theming, and what the file operations guarantee.

The design is [docs/specs/2026-09-27-manycommander-design.md](docs/specs/2026-09-27-manycommander-design.md);
the implementation plan and its execution record are in [docs/plans/](docs/plans/).

## Quick start

Linux 6.8 or later and a stable Rust toolchain:

```bash
cargo install --git https://github.com/manyfold-dk/manycommander --root ~/.local
```

Then bind it in `~/.config/hypr/bindings.lua` and press `SUPER + E`:

```lua
o.bind("SUPER + E", "File manager (dual pane)", { tui = "manycommander", focus = true })
```

`F1` shows the keymap. The user documentation lives in [site/content/docs/](site/content/docs/)
and is published at [manycommander.app/docs](https://manycommander.app/docs/); change it
there, not here.

## Development

```bash
scripts/install-hooks.sh     # the pre-push hook runs scripts/check.sh full
scripts/check.sh quick       # format, lint, unit tests
scripts/check.sh full        # every test (skips fail), cargo-deny, publication gate
scripts/bench/run.sh         # A-P-1 to A-P-7; results in docs/perf/history.md
```

`full` expects `MC_XDEV_DIR` on a second filesystem (default `/dev/shm/mc-xdev`), btrfs under
`target/`, and user namespaces (`unshare -rm`) for the bind-mount tests.

The site is a [Zola](https://www.getzola.org/) project in [site/](site/), served by a
Cloudflare Worker ([site/worker.js](site/worker.js)). The workflow
[.github/workflows/site.yml](.github/workflows/site.yml) checks it on every change and
deploys it from `main`; the one-time setup is
[docs/runbooks/site-first-deploy.md](docs/runbooks/site-first-deploy.md).

```bash
scripts/site.sh serve        # live preview on http://localhost:1111
scripts/site.sh check        # build and check internal links
scripts/site.sh worker       # wrangler deploy --dry-run, then the Worker's redirect and headers
cargo run --example site_screens   # regenerate site/static/screens/ from the installed themes
```

## License

Apache-2.0. See [LICENSE](LICENSE).
