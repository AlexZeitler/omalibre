#!/bin/bash
#
# Wires omalibre's colours into Omarchy.
#
# It links one template below ~/.config/omarchy/themed/. That directory belongs
# to you and an Omarchy update never touches it. Because it is a symlink and not
# a copy, a later "git pull" is all it takes to update.

set -euo pipefail

REPO_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)

command -v omarchy-theme-set >/dev/null || {
  echo "Omarchy not found: omarchy-theme-set is not in PATH."
  echo "omalibre works without it and falls back to built-in colours."
  exit 1
}

mkdir -p ~/.config/omarchy/themed
ln -snf "$REPO_DIR/themed/omalibre.toml.tpl" ~/.config/omarchy/themed/omalibre.toml.tpl
echo "Linked ~/.config/omarchy/themed/omalibre.toml.tpl"

# Re-apply the current theme so omalibre.toml is generated right away. Without
# this, nothing happens until the next theme switch. The wallpaper stays as it is.
#
# Omarchy 4 ("Quattro") keeps the theme name in the state directory, earlier
# releases keep it in the config directory.
theme=""
for name_file in ~/.local/state/omarchy/current/theme.name ~/.config/omarchy/current/theme.name; do
  [[ -f $name_file ]] && theme=$(cat "$name_file") && break
done

[[ -n $theme ]] || {
  echo "No current Omarchy theme found. Set a theme, then run this again."
  exit 1
}

echo "Re-applying theme '$theme' ..."
OMARCHY_THEME_SKIP_BACKGROUND=1 omarchy-theme-set "$theme"

for dir in ~/.local/state/omarchy/current/theme ~/.config/omarchy/current/theme; do
  if [[ -f $dir/omalibre.toml ]]; then
    echo
    echo "Generated $dir/omalibre.toml:"
    sed -n '/\[colors\]/,$p' "$dir/omalibre.toml"
    exit 0
  fi
done

echo "The template did not produce a file. Check whether the theme ships an"
echo "omalibre.toml of its own, which would take precedence."
exit 1
