#!/usr/bin/env bash
# install-hooks.sh -- install the repository's own pre-push hook, which runs
# `scripts/check.sh full`. A global core.hooksPath hook, where present, is expected to call
# this hook ($(git rev-parse --git-common-dir)/hooks/pre-push) before its own checks.
set -euo pipefail
top="$(git rev-parse --show-toplevel)"
hook="$(git -C "$top" rev-parse --path-format=absolute --git-common-dir)/hooks/pre-push"
marker="# Installed by scripts/install-hooks.sh"
if [ -e "$hook" ] && ! grep -qF "$marker" "$hook"; then
  echo "install-hooks: $hook exists and was not installed by this script; leaving it alone" >&2
  exit 1
fi
mkdir -p "$(dirname "$hook")"
cat > "$hook" <<'HOOK'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh: the local gate runs before every push.
set -euo pipefail
exec "$(git rev-parse --show-toplevel)/scripts/check.sh" full
HOOK
chmod +x "$hook"
echo "installed $hook"
