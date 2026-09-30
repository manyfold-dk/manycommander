# Publish the AUR package manycommander-bin for the first time

This runbook connects the aur workflow (`.github/workflows/aur.yml`) to the AUR. The aur
workflow builds the package `manycommander-bin` from a GitHub release and pushes the package
to the AUR. The release workflow calls the aur workflow after each release. The push step
runs only when the repository variable `AUR_PUBLISH` is `true`.

Use this procedure again to rebuild the setup, for example after the loss of the SSH key.

## Prerequisites

- An account on `https://aur.archlinux.org`. The AUR shows the account name as the
  maintainer of the package.
- Admin access to the GitHub repository `manyfold-dk/manycommander`.
- The `gh` command, logged in to GitHub.
- The `ssh-keygen` command.
- For the verification: a computer with Omarchy or Arch Linux, and `yay`.

## Pre-action checklist

- [ ] The AUR has no package `manycommander-bin` yet. This command prints `"resultcount":0`:

  ```bash
  curl -s 'https://aur.archlinux.org/rpc/v5/info?arg[]=manycommander-bin'
  ```

- [ ] A test run of the aur workflow without the push is green. Start the test run:

  ```bash
  tag=$(gh release view -R manyfold-dk/manycommander --json tagName -q .tagName)
  gh workflow run aur.yml -R manyfold-dk/manycommander -f tag="$tag" -f push=false
  ```

  Then watch the test run with `gh run watch -R manyfold-dk/manycommander`.

## Procedure

### Step 1: Create the SSH key

> **Warning:** The private key gives write access to every AUR package of the account. Use a
> new key for the workflow only. Do not put the private key in a file that you keep, a chat
> or a terminal command.

1. Make a temporary directory:

   ```bash
   dir=$(mktemp -d)
   ```

2. Create the key pair in the temporary directory:

   ```bash
   ssh-keygen -t ed25519 -N '' -C 'manycommander-bin (GitHub Actions)' -f "$dir/aur"
   ```

### Step 2: Add the public key to the AUR account

1. Log in to `https://aur.archlinux.org`.
2. Open **My Account**.
3. Show the public key with `cat "$dir/aur.pub"`.
4. Add the line from `cat` to the field **SSH Public Key**. Keep the lines of other keys.
5. Type the account password in the field **Your current password**.
6. Click **Update**.

### Step 3: Store the private key in GitHub

1. Send the private key into the repository secret `AUR_SSH_PRIVATE_KEY`:

   ```bash
   gh secret set AUR_SSH_PRIVATE_KEY -R manyfold-dk/manycommander < "$dir/aur"
   ```

2. Delete the temporary directory:

   ```bash
   rm -rf "$dir"
   ```

### Step 4: Enable the push

1. Set the repository variable `AUR_PUBLISH` to `true`:

   ```bash
   gh variable set AUR_PUBLISH -R manyfold-dk/manycommander --body true
   ```

### Step 5: Push the latest release

1. Find the tag of the latest release:

   ```bash
   tag=$(gh release view -R manyfold-dk/manycommander --json tagName -q .tagName)
   ```

2. Start the aur workflow with the push:

   ```bash
   gh workflow run aur.yml -R manyfold-dk/manycommander -f tag="$tag" -f push=true
   ```

3. Watch the run with `gh run watch -R manyfold-dk/manycommander`.
4. Confirm that the step **Push to the AUR** is green.

The first push creates the package on the AUR. Each later release pushes a new version
automatically.

### Step 6: Add the AUR install to the documentation

1. Add a section **Install from the AUR** to `site/content/docs/install.md`, after the section
   **Install with mise**. Give the command `yay -S manycommander-bin`.
2. Add the AUR package to the section `[Unreleased]` in `CHANGELOG.md`.

## Verification

1. Open `https://aur.archlinux.org/packages/manycommander-bin`.
2. Confirm that the version is the tag without the `v`, followed by `-1`.
3. On the Omarchy computer, install the package:

   ```bash
   yay -S manycommander-bin
   ```

4. Run `/usr/bin/manycommander --version`.
5. Confirm that the output shows the version of step 2 without the `-1`.

Keep one install of manycommander on a computer. With a mise install and the AUR package
together, a shell and the Hyprland session can start different copies. To remove a mise
install, run `mise unuse -g github:manyfold-dk/manycommander`.

## Rollback

- To stop the automatic push, set the variable `AUR_PUBLISH` to `false`:

  ```bash
  gh variable set AUR_PUBLISH -R manyfold-dk/manycommander --body false
  ```

- To revoke the key, remove the key line from **My Account** on the AUR. Then delete the
  secret with `gh secret delete AUR_SSH_PRIVATE_KEY -R manyfold-dk/manycommander`.
- To correct a package without a new release, fix `contrib/aur/manycommander-bin/PKGBUILD.in`
  first. Then run the aur workflow with the same tag, `-f pkgrel=2` and `-f push=true`.
- To remove the package from the AUR, open the package page and click **Submit Request**.
  Select the request type **Deletion**. The AUR package maintainers do the deletion.
