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

- `spec.rs`: `SourceSpec` (file, cmd, docker, compose, k8s, journal, stdin), parsed from `type:arg` CLI arguments (a bare argument is a file, `-` is stdin). Short prefixes (`d:`, `dc:`) exist on the CLI only, because they are quick to type. Config files keep the explicit `type` names on purpose (the user's choice). `SourceConfig` is a `SourceSpec` (flattened, tagged by `type`) plus options any source can have: `name`, `lines`, `restart` (defaults to on for docker, compose and k8s).
- `config.rs`: loads `-c file.toml` (`[[source]]` tables plus global options; relative paths resolve against the file) and merges it with the CLI into `Settings`. Config sources come first, and CLI options win.
- `source/`: the `Source` trait (`spawn(self: Box<Self>, Emitter) -> JoinHandle`), with each source running as its own tokio task and sending `SourceEvent::{Line, Reset, Status}`. `expand()` turns a source into several panes before startup (compose: one per service, listed with `docker compose config --services`). `build()` then maps each `SourceConfig` to an implementation. **This is where a new source type goes.**
  - `file.rs`: `FileFollower` is the synchronous core, with one `poll()` per wake-up. It handles the initial `-n` lines, truncation (size < position), in-place rewrites by editors (the last 256 bytes read no longer match what is on disk, even when the file grew), rotation (device and inode change: it drains the old file first) and a missing file (`Waiting` status). Truncation and rewrite emit `SourceEvent::Reset(reason)`, which the app turns into a `ClearMark`. `FileSource` wakes it through `notify` on the parent directory, plus a 1s polling fallback. Tests drive `FileFollower` directly.
  - `command.rs`: `CommandSource` runs an `Argv` closure that rebuilds the command line for each run, given when the previous run ended. Each run gets its own process group (killed with `killpg` on drop, so `a | b` and background jobs die too) and stdin set to null (the TUI owns the terminal). stdout and stderr are merged, and the exit status is reported. With `restart`, it starts again with a 1s to 30s backoff. The app counts restarts (↻n in the title) when it sees `Running` after `Exited` or `Error`.
  - `tools.rs`: the `Argv` builders for docker, compose, kubectl and journalctl. The first run uses `--tail N`, and restarts use `--since <end of previous run>` so the tail is not replayed. These are CLI-backed POCs; native clients (bollard, kube-rs) are planned behind the same trait. Note: `docker compose logs -f` keeps following through container restarts by itself, whereas `docker logs -f` exits.
  - `stdin.rs`: reads on a plain std thread, so that a blocked read cannot stop the runtime from shutting down. crossterm reads keys from `/dev/tty` when stdin is a pipe.
- `color/`: mtr's own coloring. `scheme.rs` holds `Rule`s (a regex, a group, a style) compiled into a `Scheme`, which uses a `RegexSet` prefilter and lets the first rule win on overlap. `Library` holds the built-in schemes (`builtin.toml`, embedded, in the same format as the user's `[scheme.<name>]` tables) plus the user's, and chains several schemes into one. `detect.rs` guesses `json`/`http`/`syslog`/`logfmt` from a source's first 20 lines. `PaneColor` holds a pane's state (`Choice::Auto|Named|Off`, enabled, label), and the app feeds it lines through `observe()`. `style.rs` maps style names to the fixed palette. Coloring happens at render time in `ui/text.rs::rows`. A line containing ESC keeps the source's own ANSI colors, unless `--strip-ansi` is set, in which case the scheme applies to the stripped text.
- `buffer.rs`: one `VecDeque` history per source, with **stable sequence numbers** that survive eviction. Clearing a pane (`N`/`n`) only pushes a `ClearMark`, so panes show lines from `view_start()` on while the scrollback still shows everything, with marks drawn as separators. Views refer to lines by `Seq`, never by index.
- `app.rs`: `App` holds the state (`Mode::{Panes, Select, Scrollback}`) and key handling (`handle_key`, unit-tested without a terminal). `run()` is the `tokio::select!` loop over crossterm's `EventStream`, source events and SIGTERM, SIGHUP and SIGINT. A signal quits cleanly, so that dropping the sources stops their processes. Events arrive in batches and the screen redraws only when something changed.
- `ui/`: drawing only. Its one write back to state is that the scrollback renderer normalizes `Scrollback.top`: it clamps it to the history, sets `None` (follow the end) once the end is visible, and records `shown_top` and `page` for the next key press. `layout.rs` has the `PaneLayout` trait (only `Stacked` exists today). `text.rs` parses ANSI (with `ansi-to-tui`, or strips it with `--strip-ansi`) and wraps by character, which keeps row counts exact for bottom anchoring. UI tests render the full screen with ratatui's `TestBackend`. mtr's own chrome only uses the fixed 256-palette constants at the top of `ui/mod.rs`, never named colors (`Color::Blue`…), because terminal themes remap those. Log content keeps the terminal defaults and its own ANSI colors.

## Keys (multitail-compatible where possible)

`q`/`Esc` quit (or leave a popup or the scrollback), `Ctrl+C` quits from anywhere. `N` clears all panes, `n` opens a picker to clear one. `b` opens a picker, then shows the scrollback (`j`/`k`, `PgUp`/`PgDn`, `g`/`G`). `C` toggles coloring on all panes, and `c` opens a picker to toggle it on one (multitail's `c` also works per window). With a single pane, the picker is skipped.

## Demo

`demo/` has a compose project (`mtrdemo`): `web` (plain), `db` (its own ANSI colors), `nginx` (access logs), `api` (JSON) and `worker` (logfmt). `demo/stack.toml` follows all of them, with a custom scheme on `web`, plus CoreDNS on the default kube context (OrbStack locally). Only read-only kube commands are run against that cluster.

## Roadmap context

Milestones done: 1 (core TUI), 2 (sources: config file, compose, k8s, journal, stdin, restart) and coloring (schemes, detection, c/C). Next is "navigation" (search, regex filters, `B` merged scrollback). Explicitly deferred: native docker and kubernetes, other layouts (first idea: shrink empty or waiting panes), formatters (pretty JSON, field selection), value-based colors (multitail's `cs_re_val_*`), mouse support.
