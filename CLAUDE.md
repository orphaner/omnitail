# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`mtr` is a modern Rust clone of `multitail`: one TUI showing several log streams at once in stacked panes. Unlike multitail it is built around pluggable *sources*: files, arbitrary commands, docker, and later kubernetes and other streams. `specs.md` holds the user's goals. The multitail C sources at `/Users/nicolas/geekmac/multitail` are the reference for behavior and key bindings (for example `mt.c` for the key dispatch). The user writes in French.

## Commands

```bash
cargo build
cargo test                    # all tests; a single one: cargo test <name filter>, e.g. cargo test rotation
cargo clippy --all-targets    # expected to be warning-free
cargo run -- _geek/small.txt 'cmd:ping -i 0.5 127.0.0.1' docker:<container>
```

The app needs a real TTY. mtr's own logs go to a file (`--log-file`, `$TMPDIR/mtr.log` by default, level set by `MTR_LOG`), never to stdout or stderr. Sample inputs are in `_geek/`.

To check the real binary without an interactive terminal, run it inside `script` with an explicit size and keys piped in, then replay the capture with a terminal emulator such as Python `pyte`:
`(sleep 1; printf b; sleep .3; printf q) | script -q out.tty sh -c 'stty rows 30 cols 100; exec ./target/debug/mtr …'`.
Without `stty`, the pty is 0×0 and nothing is drawn.

## Architecture

The data flows one way: **sources → bounded mpsc channel → `App` state → `ui` draws it**.

- `spec.rs`: `SourceSpec`, parsed from `type:arg` CLI arguments (a bare argument is a file). It also derives `serde::Deserialize` (internally tagged by `type`), so a TOML source file will load into the same type.
- `source/`: the `Source` trait (`spawn(self: Box<Self>, Emitter) -> JoinHandle`), with each source running as its own tokio task and sending `SourceEvent::{Line, Status}`. `build()` maps a spec to an implementation. **This is where a new source type goes.**
  - `file.rs`: `FileFollower` is the synchronous core, with one `poll()` per wake-up. It handles the initial `-n` lines, truncation (size < position), rotation (device and inode change: it drains the old file first) and a missing file (`Waiting` status). `FileSource` wakes it through `notify` on the parent directory, plus a 1s polling fallback. Tests drive `FileFollower` directly.
  - `command.rs`: runs through `sh -c`, with stdin set to null (the TUI owns the terminal) and `kill_on_drop`. stdout and stderr are merged, and the exit status is reported. `docker:` is currently just `docker logs --follow --tail N` through this source. The plan is native clients later (bollard, kube-rs), behind the same trait.
- `buffer.rs`: one `VecDeque` history per source, with **stable sequence numbers** that survive eviction. Clearing a pane (`N`/`n`) only pushes a `ClearMark`, so panes show lines from `view_start()` on while the scrollback still shows everything, with marks drawn as separators. Views refer to lines by `Seq`, never by index.
- `app.rs`: `App` holds the state (`Mode::{Panes, Select, Scrollback}`) and key handling (`handle_key`, unit-tested without a terminal). `run()` is the `tokio::select!` loop over crossterm's `EventStream` and source events. Events arrive in batches and the screen redraws only when something changed.
- `ui/`: drawing only. Its one write back to state is that the scrollback renderer normalizes `Scrollback.top`: it clamps it to the history, sets `None` (follow the end) once the end is visible, and records `shown_top` and `page` for the next key press. `layout.rs` has the `PaneLayout` trait (only `Stacked` exists today). `text.rs` parses ANSI (with `ansi-to-tui`, or strips it with `--strip-ansi`) and wraps by character, which keeps row counts exact for bottom anchoring. UI tests render the full screen with ratatui's `TestBackend`.

## Keys (multitail-compatible where possible)

`q`/`Esc` quit (or leave a popup or the scrollback), `Ctrl+C` quits from anywhere. `N` clears all panes, `n` opens a picker to clear one. `b` opens a picker, then shows the scrollback (`j`/`k`, `PgUp`/`PgDn`, `g`/`G`). With a single pane, the picker is skipped.

## Roadmap context

Milestone 1 (above) is done. Explicitly deferred: loading the TOML source file, search and regex filters, `B` (merged scrollback), native docker and kubernetes, other layouts, mtr's own coloring, mouse support.
