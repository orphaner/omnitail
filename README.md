# mtr

A modern take on [multitail](https://www.vanheusden.com/multitail/), in Rust: follow several log streams at once, in stacked panes, from files, commands or containers.

## Usage

```bash
cargo run -- [OPTIONS] <SOURCE>...

# examples
mtr /var/log/system.log app.log
mtr app.log 'cmd:kubectl logs -f deploy/api' docker:postgres
```

Sources:

| Syntax | What it follows |
|---|---|
| `path` or `file:path` | a file, like `tail -F` (survives truncation, rotation, the file not existing yet) |
| `cmd:'command'` | stdout and stderr of a shell command |
| `docker:container` | `docker logs --follow` of a container |

Options: `-n/--lines N` (existing lines shown at start; by default, enough to fill each pane), `--history N` (lines kept per source, 10,000 by default), `--strip-ansi`, `--log-file PATH`.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit (or go back from a popup or the scrollback) |
| `Ctrl+C` | quit from anywhere |
| `N` | clear all panes (the history is kept) |
| `n` | choose one pane to clear |
| `b` | choose a pane and browse its whole history, including what was cleared (`j`/`k`, `PgUp`/`PgDn`, `g`/`G`) |
