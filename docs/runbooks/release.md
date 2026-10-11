# Publish a release

A release commit on `main` publishes a release. The release commit sets the new version in
`Cargo.toml` and gives the version a section in `CHANGELOG.md`. `<version>` is the new
version in the form `MAJOR.MINOR.PATCH`, without the `v`. Three workflows do the work
([ADR 0035](../adr/0035-release-on-a-new-version.md)):

| Workflow | What the workflow does |
|---|---|
| `ci` (`.github/workflows/ci.yml`) | Runs on every push to `main` and on every pull request. Runs `scripts/check.sh ci` |
| `release` (`.github/workflows/release.yml`) | Runs after each `ci` run that passed on a push to `main`, or by hand on `main` as a dry run. The job `plan` reads the version in `Cargo.toml` at the commit. If the version has a release, the run stops there. If not, the job `build` builds the binary with `cargo build --release --locked` for `x86_64-unknown-linux-gnu` with no right to publish, and packages `manycommander-<version>-x86_64-linux.tar.gz` with `LICENSE-APACHE`, `LICENSE-MIT`, `THIRD-PARTY-LICENSES`, `README.md` and `CHANGELOG.md` and its `.sha256` file. The job `publish` checks the tarball against its `.sha256`, adds a build provenance attestation, and publishes the GitHub release with the section of `<version>` in `CHANGELOG.md` as its notes. The release makes the tag `v<version>` at the commit ([ADR 0030](../adr/0030-release-in-two-jobs.md)) |
| `aur` (`.github/workflows/aur.yml`), called by `release` after `publish` | Builds the AUR package `manycommander-bin` from the release and the PKGBUILD template of its tag, checks the package with `namcap`, installs the package and runs `manycommander --version`. Pushes the package to the AUR only when the repository variable `AUR_PUBLISH` is `true`. [Set up the AUR package](aur-package-setup.md) gives the setup |

> **Warning:** A push of a release commit to `main` publishes the release when the `ci` run
> of the commit passes. A release tag cannot be moved or deleted. Push the release commit
> only when you want to publish the version.

## Prerequisites

- Push access to `manyfold-dk/manycommander`.
- A checkout of `main` with the pre-push hook of `scripts/install-hooks.sh`.
- The environment of `scripts/check.sh full`: a second filesystem for `MC_XDEV_DIR`, btrfs
  under `target/`, user namespaces, FUSE, and the name list of the publication gate in
  `MC_PUBLISH_NAMES`. The header of `scripts/check.sh` lists each item.
- The `gh` command, logged in to GitHub.
- For Step 7: the prerequisites of [Verify a release by hand](release-verification.md).

## Pre-action checklist

- [ ] `git status` shows no changed file and no unpushed commit.
- [ ] The last `ci` run on `main` is green:
      `gh run list --repo manyfold-dk/manycommander --workflow ci.yml --branch main --limit 1`.
- [ ] `CHANGELOG.md` names every change for users since the last release.
- [ ] No other release is in progress: `gh run list --repo manyfold-dk/manycommander --workflow release.yml --limit 1`
      shows no run with the status `in_progress`.

## Procedure

### Step 1: Update the checkout

1. Run `git switch main`.
2. Run `git pull --rebase`.
3. Run `scripts/check.sh full`.
4. Confirm that the last line is `check.sh full: PASS`.

### Step 2: Choose the version

1. Show the tag of the last release:

   ```bash
   git describe --tags --abbrev=0
   ```

2. Read the changes since the last release in `CHANGELOG.md`.
3. Choose the version part to raise:

   | Changes since the last release | Raise |
   |---|---|
   | A new feature, a new key or a change in behaviour | The minor version. The patch version becomes `0` |
   | Fixes, tests and documentation only | The patch version |
   | A change that breaks a configuration file or saved state | The major version. While the major version is `0`, raise the minor version |

### Step 3: Prepare the release commit

1. In `Cargo.toml`, set `version = "<version>"` in the `[package]` table.
2. Run `cargo check`. Cargo writes the new version into `Cargo.lock`.
3. Run `git diff Cargo.lock`. Confirm that only the `version` line of the package
   `manycommander` changes.
4. In `CHANGELOG.md`, find the section `## [Unreleased]`.
5. If `CHANGELOG.md` has no section `## [Unreleased]`, add the heading above the newest version
   section. Write each change since the last release under `### Added`, `### Changed` or
   `### Fixed`, as in the earlier sections.
6. Replace the heading `## [Unreleased]` with `## [<version>] - <date>`. `<date>` is the date
   of today in the form `YYYY-MM-DD`.
7. Below the new heading, add one sentence that sums up the release for a user. The earlier
   sections show the form.
8. Stage the three files:

   ```bash
   git add Cargo.toml Cargo.lock CHANGELOG.md
   ```

9. Commit with the subject `chore(release): prepare <version>`. In the body, write what the
   release contains and why you raised that version part.

The release workflow takes the release notes from the section `## [<version>]`. Without that
section, the release workflow stops before it publishes.

### Step 4: Push the release commit

1. Run `git pull --rebase && git push`. The pre-push hook runs `scripts/check.sh full` again.
2. If GitHub refuses the push because `main` moved, run `git log HEAD..origin/main`.
3. If the new commits change the program, add the commits to the changelog section. Amend
   the release commit. Then do item 1 again.
4. Record the commit of the release:

   ```bash
   sha="$(git rev-parse HEAD)"
   ```

### Step 5: Watch the workflows

1. Wait for the `ci` run of the commit:

   ```bash
   run="$(gh run list --repo manyfold-dk/manycommander --workflow ci.yml --commit "$sha" --limit 1 --json databaseId --jq '.[0].databaseId')"
   gh run watch "$run" --repo manyfold-dk/manycommander --exit-status
   ```

2. If the `ci` run fails, no release starts. Fix the cause in a new commit on `main`. The
   `ci` run of that commit starts the release.
3. Wait for the `release` run. The run starts when the `ci` run ends:

   ```bash
   run="$(gh run list --repo manyfold-dk/manycommander --workflow release.yml --event workflow_run --limit 1 --json databaseId --jq '.[0].databaseId')"
   gh run watch "$run" --repo manyfold-dk/manycommander --exit-status
   ```

4. Confirm that the jobs `plan`, `build`, `publish` and `aur / package` are green.
5. Look at the step **Push to the AUR** of `aur / package`. Before the AUR setup, the step
   shows `skipped`. A skipped step is the expected result.
6. Confirm that the tag points at the release commit. The command prints `$sha`:

   ```bash
   git fetch --tags origin && git rev-parse "v<version>^{commit}"
   ```

### Step 6: Check the published release

1. Show the release:

   ```bash
   gh release view "v<version>" --repo manyfold-dk/manycommander
   ```

2. Confirm that the release has two assets: the tarball and its `.sha256` file.
3. Confirm that the notes are the section `## [<version>]` of `CHANGELOG.md`.
4. Download the assets into a new directory:

   ```bash
   tmp="$(mktemp -d)"
   gh release download "v<version>" --repo manyfold-dk/manycommander --pattern '*-x86_64-linux.tar.gz*' --dir "$tmp"
   ```

5. Check the checksum. The output is `OK` for the tarball:

   ```bash
   (cd "$tmp" && sha256sum -c ./*.sha256)
   ```

6. Check the build provenance attestation. The output names the `release` workflow of
   `manyfold-dk/manycommander`:

   ```bash
   gh attestation verify "$tmp"/manycommander-*-x86_64-linux.tar.gz --repo manyfold-dk/manycommander
   ```

7. Run the binary. The output is `manycommander <version>`:

   ```bash
   tar -xzf "$tmp"/manycommander-*-x86_64-linux.tar.gz -C "$tmp"
   "$tmp"/manycommander-*-x86_64-linux/manycommander --version
   ```

8. Confirm that the tarball has the licence files. The output shows `LICENSE-APACHE`,
   `LICENSE-MIT` and `THIRD-PARTY-LICENSES`:

   ```bash
   ls "$tmp"/manycommander-*-x86_64-linux/
   ```

9. Remove the directory: `rm -rf "$tmp"`.

### Step 7: Verify the release by hand

1. Do [Verify a release by hand](release-verification.md). After a minor release, do the full
   runbook. After a patch release, do the parts that the changes of the release touch.
2. For each `fail`, open an issue. A fail that blocks users needs a patch release.

### Build without publishing

To check a build before you push a release commit, run the release workflow by hand on
`main`. The run builds and packages `main` and publishes nothing:

```bash
gh workflow run release.yml --repo manyfold-dk/manycommander --ref main
```

## Verification

1. Run the command below. The output is `v<version>`:

   ```bash
   gh release view --repo manyfold-dk/manycommander --json tagName --jq .tagName
   ```

2. Open <https://github.com/manyfold-dk/manycommander/releases>. Confirm that the release shows
   as `Latest`.

## Rollback

> **Warning:** A release tag cannot be moved or deleted. The repository ruleset "release
> tags" refuses both for every `v*` tag, also for administrators. A failed or broken release
> therefore uses up its version.

- **The release run fails in `plan` or `build`.** No release and no tag exist. Fix the cause
  in a new commit on `main`. When the `ci` run of that commit passes, the release starts
  again with the same version.
- **The release run fails in `publish`.** Rerun the failed jobs:

  ```bash
  gh run rerun "$run" --repo manyfold-dk/manycommander --failed
  ```

  If the tag `v<version>` exists and the fix needs a new commit, the version is used up. In
  `Cargo.toml` and `CHANGELOG.md`, change the version to the next patch version. Then start
  again at Step 3.
- **The published release is broken, and nobody downloaded it.** Each asset shows a download
  count of `0`:

  ```bash
  gh release view "v<version>" --repo manyfold-dk/manycommander --json assets --jq '.assets[] | [.name, .downloadCount] | @tsv'
  ```

  Delete the release. The tag stays:

  ```bash
  gh release delete "v<version>" --repo manyfold-dk/manycommander --yes
  ```

  The tag stays, so the version is used up. In the same push as the fix, change the
  version in `Cargo.toml` and `CHANGELOG.md` to the next patch version. Until that push, the
  `release` run of each push to `main` fails in `plan`, because the tag is at another commit.
- **The published release is broken, and somebody downloaded it.** Keep the release. Fix the
  cause, and publish a patch release with this runbook. Then add a line to the notes of the
  broken release that names the fixed version.
- **The AUR package is broken, and the program is correct.** Follow the rollback of
  [Set up the AUR package](aur-package-setup.md#rollback): push a fixed package with a higher
  `pkgrel`.
