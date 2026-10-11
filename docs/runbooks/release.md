# Publish a release

A tag `v<version>` on `main` publishes a release. `<version>` is the new version in the form
`MAJOR.MINOR.PATCH`, without the `v`. The tag starts two workflows:

| Workflow | What the workflow does |
|---|---|
| `release` (`.github/workflows/release.yml`) | Runs only for a tag of the form `vMAJOR.MINOR.PATCH`. Checks that the tagged commit is on `main`. Checks that the tag equals `v` and the `version` in `Cargo.toml`. Builds the binary with `cargo build --release --locked` for `x86_64-unknown-linux-gnu`. Packages `manycommander-<version>-x86_64-linux.tar.gz` with `LICENSE`, `README.md` and `CHANGELOG.md`, and writes its `.sha256` file. Adds a build provenance attestation to the tarball. Takes the release notes from the section of `<version>` in `CHANGELOG.md`. Publishes the GitHub release |
| `aur` (`.github/workflows/aur.yml`), called by `release` | Builds the AUR package `manycommander-bin` from the release, checks the package with `namcap`, installs the package and runs `manycommander --version`. Pushes the package to the AUR only when the repository variable `AUR_PUBLISH` is `true`. [Set up the AUR package](aur-package-setup.md) gives the setup |
| `ci` (`.github/workflows/ci.yml`) | Runs on every push to `main` and on every pull request. Runs `scripts/check.sh ci` |

> **Warning:** The release workflow refuses a tag on a commit that is not on `main`. The
> release workflow does not check that the `ci` run of the commit passed. Do that check
> yourself in Step 4 and Step 5.

## Prerequisites

- Push access to `manyfold-dk/manycommander`, for commits and tags.
- A checkout of `main` with the pre-push hook of `scripts/install-hooks.sh`.
- The environment of `scripts/check.sh full`: a second filesystem for `MC_XDEV_DIR`, btrfs
  under `target/`, user namespaces, FUSE, and the name list of the publication gate in
  `MC_PUBLISH_NAMES`. The header of `scripts/check.sh` lists each item.
- The `gh` command, logged in to GitHub.
- For Step 8: the prerequisites of [Verify a release by hand](release-verification.md).

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

### Step 4: Push the release commit and wait for CI

1. Run `git pull --rebase && git push`. The pre-push hook runs `scripts/check.sh full` again.
2. If GitHub refuses the push because `main` moved, run `git log HEAD..origin/main`.
3. If the new commits change the program, add the commits to the changelog section. Amend
   the release commit. Then do item 1 again.
4. Record the commit of the release:

   ```bash
   sha="$(git rev-parse HEAD)"
   ```

5. Wait for the `ci` run of the commit:

   ```bash
   run="$(gh run list --repo manyfold-dk/manycommander --workflow ci.yml --commit "$sha" --limit 1 --json databaseId --jq '.[0].databaseId')"
   gh run watch "$run" --repo manyfold-dk/manycommander --exit-status
   ```

6. Confirm that the run ends green. If the run fails, fix the cause in a new commit on
   `main`. Then start again at Step 4. Do not tag a commit with a failed `ci` run.

### Step 5: Tag the release commit

> **Warning:** A tag on the wrong commit publishes the wrong program. Check the commit before
> you push the tag.

1. Run `git fetch origin`.
2. Confirm that the commit is on `main`. The command prints `on main`:

   ```bash
   git merge-base --is-ancestor "$sha" origin/main && echo "on main"
   ```

3. Confirm that `Cargo.toml` at the commit has the new version:

   ```bash
   git show "$sha:Cargo.toml" | grep -m1 '^version'
   ```

4. Make the annotated tag:

   ```bash
   git tag -a "v<version>" -m "manycommander <version>" "$sha"
   ```

5. Push the tag. The pre-push hook runs `scripts/check.sh full` again:

   ```bash
   git push origin "v<version>"
   ```

### Step 6: Watch the release workflow

1. Wait for the release run:

   ```bash
   run="$(gh run list --repo manyfold-dk/manycommander --workflow release.yml --commit "$sha" --limit 1 --json databaseId --jq '.[0].databaseId')"
   gh run watch "$run" --repo manyfold-dk/manycommander --exit-status
   ```

2. Confirm that the job `release` is green.
3. Confirm that the job `aur / package` is green.
4. Look at the step **Push to the AUR** of `aur / package`. Before the AUR setup, the step
   shows `skipped`. A skipped step is the expected result.

### Step 7: Check the published release

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

8. Remove the directory: `rm -rf "$tmp"`.

### Step 8: Verify the release by hand

1. Do [Verify a release by hand](release-verification.md). After a minor release, do the full
   runbook. After a patch release, do the parts that the changes of the release touch.
2. For each `fail`, open an issue. A fail that blocks users needs a patch release.

## Verification

1. Run the command below. The output is `v<version>`:

   ```bash
   gh release view --repo manyfold-dk/manycommander --json tagName --jq .tagName
   ```

2. Open <https://github.com/manyfold-dk/manycommander/releases>. Confirm that the release shows
   as `Latest`.

## Rollback

- **The release workflow fails before it publishes.** No release exists yet. Delete the tag,
  fix the cause in a new commit on `main`, and start again at Step 4 with the same version:

  ```bash
  git push origin --delete "v<version>"
  git tag -d "v<version>"
  ```

- **The published release is broken, and nobody downloaded it.** Each asset shows a download
  count of `0`:

  ```bash
  gh release view "v<version>" --repo manyfold-dk/manycommander --json assets --jq '.assets[] | [.name, .downloadCount] | @tsv'
  ```

  Then delete the release and its tag, and do the rollback of the first item:

  ```bash
  gh release delete "v<version>" --repo manyfold-dk/manycommander --cleanup-tag --yes
  git tag -d "v<version>"
  ```

- **The published release is broken, and somebody downloaded it.** Keep the release. Fix the
  cause, and publish a patch release with this runbook. Then add a line to the notes of the
  broken release that names the fixed version.
- **The AUR package is broken, and the program is correct.** Follow the rollback of
  [Set up the AUR package](aur-package-setup.md#rollback): push a fixed package with a higher
  `pkgrel`.
