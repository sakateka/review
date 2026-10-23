# review

A terminal UI for reviewing Git diffs locally, with GitHub-style "viewed"
marks that survive restarts.

Run `review` with ordinary `git diff` arguments, go through the files, and
mark each one as reviewed. When you come back after the branches have moved,
marks stay on files whose content did not change and are dropped from files
that did.

- Any comparison `git diff` accepts: working tree, `--cached`, refs, ranges,
  merge-base, blobs, `--no-index`.
- Native rendering with syntax highlighting and intraline change emphasis.
- Go to definition, references, documentation and definition previews without
  an LSP, built on tree-sitter and [riptags](https://crates.io/crates/riptags).
- Line and range comments saved to a Markdown file you can hand to a person or
  an AI agent.
- Vim or Emacs keybindings, all remappable.
- A single binary. The only thing it needs at runtime is `git`.

## Install

Requires Rust 1.95 or newer.

```sh
cargo install --path . --locked
```

## Usage

Everything after the first `--` goes to `git diff` unchanged, including Git's
own `--` path separator:

```sh
review -- HEAD                  # working tree against HEAD
review -- --cached              # staged changes
review -- main feature          # two branches
review -- main...feature        # changes since the merge base
review -- main feature -- src/  # limited to a path
review --keymap emacs -- HEAD
```

`review --help` lists every action with its current key bindings.

## Keys

Vim keymap (default):

| Key | Action |
| --- | --- |
| `j` / `k`, arrows | Move between files, or the cursor in a diff |
| `Enter` / `Space` | Open the selected file's diff |
| `Ctrl-J` / `Ctrl-K` | Next / previous file's diff |
| `Tab` | Switch pane |
| `m` | Toggle the file's reviewed mark |
| `a` | Toggle between the diff and the new side only |
| `c` / `dc` | Add or edit / delete a comment on the line or the `v` selection |
| `gd` / `gr` | Definition / references under the cursor |
| `K` / `P` | Show the documentation / preview the definition |
| `Ctrl-O` | Go back |
| `/` | Search symbols |
| `*` / `#`, `n` / `N` | Search the word under the cursor |
| `[` / `]` | Old / new side |
| `:` | Command line, e.g. `:colorscheme Dracula` |
| `Alt-P` | Command palette |
| `Esc` / `q` | Close the view; `q` in the file list quits |
| `,q` | Quit from anywhere |

Usual Vim motions work in diffs: `w b e`, `0 ^ $`, `gg G`, `Ctrl-U/D/B/F`,
counts. The Emacs keymap uses `Ctrl-N/P/B/F`, `Alt-.`, `Alt-?`, `Alt-,` and
`Alt-X` instead. Shortcuts keep working with a Russian, Ukrainian or
Belarusian keyboard layout active, as with Vim's `langmap`.

## Review marks

Files show `-` while pending and `+` with a dimmed name once reviewed. Marks
are saved immediately to `$XDG_STATE_HOME/review/` (by default
`~/.local/state/review/`), separately for each repository and argument list.
A mark is tied to the content of both sides of a file, so it survives a rebase
or a branch moving as long as that file's diff is the same.

review never writes to the repository, the index, refs or Git configuration.

## Comments

Press `c` in a diff to comment on a line, a `v` range or the whole file (on
its header). Comments are written to `review-<arguments>.comments` in the
current directory: Markdown with front matter describing the comparison, and
the reviewed excerpt next to each comment. On restart they are loaded again,
and any whose lines have changed are flagged as outdated.

## Code navigation

Definitions, references, documentation (`K`) and definition previews (`P`)
work for Rust, C, C++, Go, Zig, Python and shell, with outlines for YAML and
JSON. Navigation is syntax-based, so results are candidates rather than
resolved bindings. Everything reads the compared revisions rather than your
checkout, so the old side works too.

Highlighting covers the same languages plus TOML through tree-sitter, and
everything else bat supports through syntect.

## Configuration

`$XDG_CONFIG_HOME/review/config.toml` (by default `~/.config/review/config.toml`),
or a file passed with `--config PATH`:

```toml
[features]
keymap = "vim"       # or "emacs"
readline = true      # Ctrl-A/E/N/P and Alt-B/F in diffs

[appearance]
colorscheme = "gruvbox-baby"

[keybindings]
toggle_mark = "m"
go_to_definition = "gd"
next_file_diff = "ctrl-j"
toggle_after_changes = ""  # an empty string disables an action

[keyboard]
layouts = ["ru", "uk", "be"]   # [] turns layout translation off
langmap = ""                   # extra Vim-style pairs, e.g. "оj,лk"

[telemetry]
enabled = true
```

The default theme is `gruvbox-baby`. `review-classic` is the other built-in
theme, and all of bat's themes (Dracula, Nord, Solarized, Catppuccin, ...) are
available as well. `:colorscheme` with Tab completion switches themes for the
current session.

## Telemetry

review appends timings of startup and code navigation to
`$XDG_STATE_HOME/review/telemetry.jsonl`, to help with performance work.
It contains only durations and counts, never paths, names or arguments.
Nothing leaves your machine, because review has no network code. The file is
rotated at 5 MiB. Turn it off with `[telemetry] enabled = false`.

## Development

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

`cargo test` includes a PTY test that drives the real binary in a terminal.
Point `REVIEW_BINARY` at an installed binary to test that one instead.

## License

MIT
