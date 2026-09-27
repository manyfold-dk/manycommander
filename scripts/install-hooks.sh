#!/usr/bin/env bash
# install-hooks.sh -- install the repository's own pre-push hook, which runs
# `scripts/check.sh full`. A global core.hooksPath hook, where present, is expected to call
# this hook ($(git rev-parse --git-common-dir)/hooks/pre-push) before its own checks.
set -euo pipefail
top="$(git rev-parse --show-toplevel)"
hook="$(git -C "$top" rev-parse --path-format=absolute --git-common-dir)/hooks/pre-push"
mkdir -p "$(dirname "$hook")"
cat > "$hook" <<'HOOK'
#!/usr/bin/env bash
# Installed by scripts/install-hooks.sh: the local gate runs before every push.
set -euo pipefail
exec "$(git rev-parse --show-toplevel)/scripts/check.sh" full
HOOK
chmod +x "$hook"
echo "installed $hook"
