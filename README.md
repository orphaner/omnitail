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
| `compose:` / `compose:web,db` | a compose project, one pane per service |
| `k8s:deploy/api [kubectl args]` | `kubectl logs --follow`; a label selector (`k8s:app=api`) merges its pods |
| `journal:` / `journal:nginx` | the systemd journal, or one unit |
| `-` | mtr's stdin (`make 2>&1 \| mtr - app.log`) |

Container sources (docker, compose, k8s) restart when their stream ends, resuming where it stopped.

Sources can also be listed in a TOML file, `mtr -c stack.toml`, with per-source `name`, `lines` and `restart`. See [`demo/stack.toml`](demo/stack.toml), which runs against the demo compose project in `demo/`.

Options: `-c/--config PATH`, `-n/--lines N` (existing lines shown at start; by default, enough to fill each pane), `--history N` (lines kept per source, 10,000 by default), `--strip-ansi`, `--log-file PATH`.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit (or go back from a popup or the scrollback) |
| `Ctrl+C` | quit from anywhere |
| `N` | clear all panes (the history is kept) |
| `n` | choose one pane to clear |
| `b` | choose a pane and browse its whole history, including what was cleared (`j`/`k`, `PgUp`/`PgDn`, `g`/`G`) |
