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
| `docker:container` or `d:container` | `docker logs --follow` of a container |
| `compose:` / `compose:web,db` (or `dc:…`) | a compose project, one pane per service |
| `k8s:deploy/api [kubectl args]` | `kubectl logs --follow`; a label selector (`k8s:app=api`) merges its pods |
| `journal:` / `journal:nginx` | the systemd journal, or one unit |
| `-` | mtr's stdin (`make 2>&1 \| mtr - app.log`) |

Container sources (docker, compose, k8s) restart when their stream ends, resuming where it stopped.

Sources can also be listed in a TOML file, `mtr -c stack.toml`, with per-source `name`, `lines` and `restart`. See [`demo/stack.toml`](demo/stack.toml), which runs against the demo compose project in `demo/`.

## Colors

mtr colors log lines itself: levels, timestamps, IPs, URLs, UUIDs and durations stand out. Each source's format is guessed from its first lines, then the matching scheme is used: `http` (nginx/apache access logs), `json`, `logfmt` or `syslog`, falling back on `generic`. Lines that already carry their own ANSI colors keep them.

In a config file, a source can pick its scheme(s) explicitly (`scheme = "http"`, `scheme = ["mine", "generic"]`, or `scheme = "none"`), and you can define your own:

```toml
[scheme.mine]
rules = [
  { regex = 'user=(\w+)', group = 1, style = "yellow bold" },  # group 0 (default) = whole match
  { regex = 'payment failed', style = "fg:white bg:160 bold" },
]
```

Styles are color names (red, green, yellow, blue, magenta, cyan, white, gray, orange, black), `0`-`255` or `#rrggbb`, plus `bold`, `dim`, `italic`, `underline` and `reverse`, with `fg:` and `bg:` prefixes. Where two rules match the same text, the first one wins. `--no-color` (or `color = false`) starts with coloring off.

Options: `-c/--config PATH`, `-n/--lines N` (existing lines shown at start; by default, enough to fill each pane), `--history N` (lines kept per source, 10,000 by default), `--strip-ansi` (then mtr colors those lines too), `--no-color`, `--log-file PATH`.

## Keys

| Key | Action |
|---|---|
| `q` / `Esc` | quit (or go back from a popup or the scrollback) |
| `Ctrl+C` | quit from anywhere |
| `N` | clear all panes (the history is kept) |
| `n` | choose one pane to clear |
| `b` | choose a pane and browse its whole history, including what was cleared |
| `P` / `p` | pause/resume all panes / one pane (lines keep arriving in the history) |
| `C` / `c` | mtr's coloring on/off for all panes / one pane |
| `w` | wrap long lines at word boundaries, or cut them (`›`) |

Uppercase keys act on all panes; lowercase ones open a picker (`1`-`9` or arrows and `Enter`).

In the history view (`b`), a cursor selects a line: `j`/`k`, `PgUp`/`PgDn`, `g`/`G` (back to following the end), `←`/`→` when lines are cut. `Enter` shows the selected line on its own, with JSON pretty-printed even after a prefix (`[pod/x] {…}`). `y` copies the line to the clipboard: locally through `pbcopy`, `wl-copy`, `xclip` or `xsel`; over SSH through OSC 52, which the terminal must allow (iTerm2: Settings → General → Selection → "Applications in terminal may access clipboard").
