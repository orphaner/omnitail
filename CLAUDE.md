# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`mtr` is an early-stage Rust TUI (ratatui + crossterm) that tails multiple files in real time, like `tail -f` with each file in its own pane. The code is exploratory: several modules are experiments or stubs, and the README describes the intended design more than what exists today.

## Commands

```bash
cargo build
cargo run          # needs a real TTY (raw mode); run from the repo root because the sample paths are relative
cargo clippy
cargo test         # no tests exist yet; run one with: cargo test <name>
```

Sample input files are in `_geek/` (`small.txt`, `medium.txt`, `large.txt`). Append to one (`echo foo >> _geek/medium.txt`) to trigger the watcher. The paths are hardcoded in `src/main.rs`. Press `q` to quit.

## Architecture

The intended design has two threads, connected by a `std::sync::mpsc` channel of `String`:

- **Watcher thread** (`src/v1.rs::run`): it uses `notify` to watch one file. An `OffsetTracker` keeps a `BufReader` open for each path, so on every modify event it reads only the lines added since the last read. This is the core "tail" mechanism.
- **UI thread** (`src/ui.rs::Ui`): it runs a ratatui loop with a 250ms tick and renders a 3-row layout (header / main pane with scrollbar / key-hint status bar). `Ui` implements `Widget for &mut Ui`. A TODO there says this should move to ratatui's state pattern.

Current gaps to know about before changing the code:
- In `v1::run`, the `tx: Sender<String>` parameter is shadowed by a local `notify` channel, so nothing ever reaches the UI. The new lines are written to stdout with `println!`, which clashes with the TUI's alternate screen.
- `Ui::start_loop` receives `rx` but never reads from it, and the main pane renders placeholder text.
- `main.rs` watches only `file_paths[1]`, even though it lists three files.

Other modules:
- `src/app.rs`: an empty `App` placeholder that is passed to `Ui::new` but not stored.
- `src/yo.rs`: a sketch of the next design. It uses a tokio `unbounded_channel` of `Action` (`AddTail`/`RemoveTail`), along with `Tail`, `State` and `InternalConfig` types. It is declared in `main.rs` but not used yet. `tokio` was added for this.
- `src/old/`: earlier iterations (`v0.rs`, `tui.rs`, `tui_v1.rs`, the latter being the multi-file vertical-split viewer the README describes). They are **not compiled** because no `mod` declaration points to them. Use them for reference only.

Some comments and log messages are in French.

## VCS

Plain git (Jujutsu was dropped). Work on `main`.
