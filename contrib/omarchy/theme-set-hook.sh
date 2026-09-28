#!/bin/bash
# manycommander theme-set hook (optional). omarchy-theme-set runs every file in
# ~/.config/omarchy/hooks/theme-set.d/ after it has retinted the apps. manycommander
# already follows theme switches through its own watcher; this hook is the fallback for
# systems where that watch cannot be placed (see site/content/docs/theme.md).
#
# Install:  cp contrib/omarchy/theme-set-hook.sh ~/.config/omarchy/hooks/theme-set.d/manycommander
pkill -USR1 -x manycommander || true
