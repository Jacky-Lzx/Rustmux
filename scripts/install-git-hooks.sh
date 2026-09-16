#!/bin/sh
set -eu

root=$(git rev-parse --show-toplevel)
common_dir=$(git rev-parse --path-format=absolute --git-common-dir)
source_hook="$root/.githooks/pre-commit"
target_hook="$common_dir/hooks/pre-commit"

if [ -e "$target_hook" ] && ! cmp -s "$source_hook" "$target_hook"; then
    if ! grep -q '^# rustmux-managed-hook$' "$target_hook"; then
        echo "Refusing to replace existing hook: $target_hook" >&2
        exit 1
    fi
fi

mkdir -p "$common_dir/hooks"
cp "$source_hook" "$target_hook"
chmod +x "$target_hook"
echo "Installed pre-commit hook at $target_hook"
