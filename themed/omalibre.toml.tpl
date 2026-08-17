# Colours for omalibre, filled in from the active Omarchy theme.
#
# omarchy-theme-set-templates renders this file on every theme change and writes
# the result to ~/.local/state/omarchy/current/theme/omalibre.toml, which the
# reader watches.
#
# Roles are mixed here rather than taken from the theme's own muted and
# lighter_background: in some themes those collapse onto foreground and
# background, which would leave dimmed text at full contrast and a code block
# with no visible backdrop. Mixing always yields a usable value.

[colors]
background = "{{ background }}"
foreground = "{{ foreground }}"

# Headings and progress.
accent = "{{ accent }}"

# Dimmed text: rules, image labels, the status line.
muted = "{{ mix background foreground 55% }}"

# Backdrop of a code listing, a touch away from the background.
code_background = "{{ mix background foreground 10% }}"
code_foreground = "{{ mix foreground accent 25% }}"

# Quotations.
quote = "{{ mix background foreground 70% }}"

# The five annotation colours.
mark_yellow = "{{ yellow }}"
mark_green = "{{ green }}"
mark_blue = "{{ blue }}"
mark_red = "{{ red }}"
mark_purple = "{{ magenta }}"
