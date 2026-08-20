---
name: omalibre
description: |
  How Omalibre is built and which of its rules break silently when you get
  them wrong: the journal is the only source of truth, a book is identified by
  the hash of its bytes, the library is a read model nobody stores, and a
  block index is the address every annotation and reading position hangs off.
  Also covers the Omarchy bar plugin in `omarchy/`, where changed QML does
  nothing until the shell restarts. Purpose: a change does not quietly
  invalidate the reading positions and notes of everyone who already uses
  this, and a plugin change is not called broken before it has been loaded.

  TRIGGER when:
  - I change anything under `src/`, `omarchy/`, `themed/` or `manifest.json`
  - I add a field to a journal event, or a new event type
  - I touch how a chapter is parsed into blocks, or how blocks are wrapped
  - I add a command line flag, or change what one prints
  - I want the panel or the bar widget to show something new
  - A plugin change shows no effect although the file is right
  - I want to see what the panel does without Omalibre installed
  - Keywords: journal, event, replay, read model, BookId, block index,
    locator, annotation, EPUB, spine, plugin, Panel.qml, manifest,
    bar widget, theme template
---

# Working on Omalibre

Omalibre is a terminal EPUB reader with a library, annotations, an MCP server
and an Omarchy bar widget. One Rust binary, one QML file, no service, no
database.

## Four sentences carry the whole design

**The journal is the truth.** Append-only JSONL, one file per machine, under
the journal directory from `paths.rs`. Two machines syncing through a shared
folder never write the same file, so no conflict copy can appear. See
`journal.rs`.

**A book is its bytes.** The identity is a hash over the file, never the path,
so a book that moves or gets renamed keeps its metadata, its reading position
and its notes. See `identity.rs`.

**The library is derived.** `library::entries` folds the replayed journal into
a list at startup and nothing stores it. Deleting the data directory costs
nothing but the fold. See `library.rs`.

**A block index is an address.** A chapter parses into a flat list of blocks,
and annotations and reading positions point at a block plus a character offset
inside it. See `doc.rs` and `annotation.rs`.

## Four rules that break in silence

**Never rename an event type, never add a required field.** `Journal::replay`
skips a line it cannot parse, without a word (`journal.rs`, in the loop around
`serde_json::from_str`). A rename does not fail the build and does not fail at
runtime. It quietly drops every event anyone has already written, and with it
their reading positions and notes. New fields go in as
`Option` with `#[serde(default)]`, the way `MetadataSet` does it.

**Block parsing must stay deterministic.** Change how `epub/xhtml.rs` splits a
chapter into blocks and every existing annotation moves to a different piece
of text, in every library, without an error anywhere. If a change is
unavoidable, say so in the commit message and think about the quote stored
alongside each highlight, which is what would let them be found again.

**Only the reader reads the journal.** The bar widget asks the binary and
draws the answer. Give something else the journal format and it becomes a
second public interface that has to hold still, while the reader needs it to
change.

**The read model is written in one direction.** Events go through
`State::apply`, and nothing else touches the maps behind it. Patch a record
from the outside and it is correct until the next start, then gone.

## Where things live at runtime

`~/.config/omalibre/config.toml` holds settings. `~/.local/share/omalibre/`
holds derived data and may be deleted at any time. The journal directory
defaults to the data directory and is meant to be pointed at a synchronised
folder. All three come from `paths.rs`; do not spell a path out anywhere else.

## The Omarchy bar plugin

`manifest.json` at the root plus `omarchy/Panel.qml` and `omarchy/omalibre-run`
make the plugin. `omarchy plugin add` clones this repo into
`~/.config/omarchy/plugins/alexzeitler.omalibre/`.

**Changed QML does nothing until the shell restarts.** The shell runs with
`QS_DISABLE_FILE_WATCHER=1`. `omarchy plugin disable`/`enable` and
`omarchy-shell shell rescanPlugins` report success and load nothing. Only
`omarchy restart shell` does, and `pgrep -f "quickshell -n"` proves it by
showing a new process id.

**Repo and installed plugin are two copies.** Copy `manifest.json`,
`omarchy/Panel.qml` and `omarchy/omalibre-run` into the plugin directory
before testing, and `diff` them before blaming your own code.

**In the panel use `bar.foreground`, never `bar.barForeground`.** The second
one is computed against the wallpaper so bar icons stay legible when the bar
is translucent. The panel paints on its own opaque surface, where that colour
is unreadable.

**Write Nerd Font glyphs as escapes.** `"\uf02d"`, not the character itself. A
literal private-use character does not survive every editor and pipe, and a
bar widget whose text ends up empty draws nothing at all, which looks exactly
like a plugin that failed to load.

**The binary is not on `PATH`.** The shell is started by the compositor, so it
inherits no login environment. `omarchy/omalibre-run` looks in `PATH`,
`~/.local/bin` and `~/.cargo/bin`, and prints `NOT_INSTALLED` on stdout rather
than failing with an exit code, because the output and the exit signal arrive
in no fixed order.

**Opening the panel takes the keyboard.** Do not summon it with
`omarchy-shell alexzeitler.omalibre open` while someone is typing: their next
keystrokes land in the search box and their Return opens a book.

**To see the not-installed state**, replace the installed copy of
`omalibre-run` with a script that only prints `NOT_INSTALLED`, restart the
shell, and put the real one back afterwards.

**QML errors go to the journal**, not to a terminal:
`journalctl --user --since "-10 min" | grep -i omalibre`.

## Colours

`themed/omalibre.toml.tpl` is a template Omarchy renders on every theme
change. `theme.rs` reads the result and falls back to built-in colours where
there is none, so the reader works without Omarchy. Working from a clone,
`./install-theme.sh` links the template instead of copying it.

## Building and checking

`cargo test` runs everything; tests live in the module they test. Pushing a
`v*` tag builds the release: a statically linked musl binary, whose archive
name carries no version so that the download URL under `releases/latest`
stays what the README says it is. See `.github/workflows/release.yml`.
