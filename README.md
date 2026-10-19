# omnitail

A modern take on [multitail](https://www.vanheusden.com/multitail/), in Rust: follow several log streams at once, in stacked panes, from files, commands or containers.

## Install

With Homebrew, on macOS or Linux (WSL included):

```bash
brew install orphaner/tap/omnitail
```

Or download an archive from the [releases](https://github.com/orphaner/omnitail/releases), or build it: `cargo install --git https://github.com/orphaner/omnitail`.

## Usage

```bash
omnitail [OPTIONS] [SOURCE]...

# examples
omnitail                       # in a directory with a compose file: all its services
omnitail /var/log/system.log app.log
omnitail app.log 'cmd:kubectl logs -f deploy/api' docker:postgres
```

Sources:

| Syntax | What it follows |
|---|---|
| `path` or `file:path` | a file, like `tail -F` (survives truncation, rotation, the file not existing yet) |
| `cmd:'command'` | stdout and stderr of a shell command |
| `docker:container` or `d:container` | `docker logs --follow` of a container |
| `compose:` / `compose:web,db` (or `dc:…`) | a compose project, one pane per service |
| `k8s:deploy/api [-n ns] [--context ctx] [-c container] [kubectl args]` | `kubectl logs --follow` of all the pods of a workload (or of a label selector, `k8s:app=api`, or a pod) and all their containers, in a pane you can split into pods, then containers |
| `journal:` / `journal:nginx` | the systemd journal, or one unit |
| `-` | omnitail's stdin (`make 2>&1 \| omnitail - app.log`) |

Container sources (docker, compose, k8s) restart when their stream ends, resuming where it stopped.

A k8s source looks at the pods its target runs when omnitail starts, and follows them all, with all their containers (native sidecars included, init containers left out; `-c app` picks one). Its pane mixes them, each line behind badges naming its pod (`xk2p9`, the end of a replica's name, one color per pod everywhere) and its container. `Enter` splits a pane into one pane per pod, then a pod's pane into one per container; `Backspace` goes back up. A single k8s source opens on its pods already (or its containers, for a single pod). Formats are guessed per container, so a pane mixing JSON and access logs colors both. `e` filters on pods and containers too. [`demo-k8s/`](demo-k8s) has workloads covering these cases.

Without any source, omnitail follows the compose project of the current directory (`compose.yaml`, `compose.yml`, `docker-compose.yaml` or `docker-compose.yml`), one pane per service. With a single source, it opens straight on its full-screen history (see [Keys](#keys)), following the end like `tail -f`; `q` quits.

Sources can also be listed in a TOML file, `omnitail -c stack.toml`, with per-source `name`, `tail`, `restart`, `scheme` and `level`. See [`demo/stack.toml`](demo/stack.toml), which runs against the demo compose project in `demo/`. `--print-config` writes one from the command line, with one source per compose service, to edit from there: `omnitail --print-config > stack.toml`.

### From k9s

[`contrib/k9s/plugins.yaml`](contrib/k9s/plugins.yaml) makes omnitail a [k9s](https://k9scli.io) plugin: `Ctrl-O` on a pod, a deployment, a statefulset, a daemonset, a replicaset, a job or a container opens omnitail on its logs, and `q` goes back to k9s. Copy it to the plugins file `k9s info` shows (or merge its entries into yours).

## Colors

omnitail colors log lines itself: levels, timestamps, IPs, URLs, UUIDs and durations stand out. Each source's format is guessed from its first lines, then the matching scheme is used: `http` (nginx/apache access logs), `json`, `logfmt` or `syslog`, falling back on `generic`. Lines that already carry their own ANSI colors keep them.

omnitail also ships 62 schemes converted from [multitail](https://github.com/folkertvanheusden/multitail) (ssh, postfix, exim, dovecot, fail2ban, apache, squid, bind, mysql, log4j, cisco_ios…): `omnitail --list-schemes` lists them all. Files whose path a scheme claims get it automatically (e.g. `/var/log/fail2ban.log` → `f2b`, `/var/log/mail/` → `postfix`).

In a config file, a source can pick its scheme(s) explicitly (`scheme = "http"`, `scheme = ["mine", "generic"]`, or `scheme = "none"`), and you can define your own:

```toml
[scheme.mine]
description = "my app"                   # shown by --list-schemes
paths = ['/var/log/myapp/']              # files picking this scheme by default
rules = [
  { regex = 'user=(\w+)', group = 1, style = "yellow bold" },  # group 0 (default) = whole match
  { regex = 'payment failed', style = "fg:white bg:160 bold" },
]
```

Styles are color names (red, green, yellow, blue, magenta, cyan, white, gray, orange, black), `0`-`255` or `#rrggbb`, plus `bold`, `dim`, `italic`, `underline` and `reverse`, with `fg:` and `bg:` prefixes. Where two rules match the same text, the first one wins. `--no-color` (or `color = false`) starts with coloring off.

Options: `-c/--config PATH`, `-n/--tail N` (existing lines read at start, kept on top of the history; by default, enough to fill each pane), `--history N` (lines kept per source since omnitail started, 10,000 by default), `--level LEVEL` (see [Levels](#levels)), `--strip-ansi` (then omnitail colors those lines too), `--no-color`, `--print-config`, `--log-file PATH`.

## Levels

`L` cycles the levels shown in all panes (all → debug and up → info and up → warn and up → errors only → all), `l` does it for one pane, and the title shows the current one (`≥warn`). `--level warn` starts every pane there, `level = "warn"` one source.

A line's level is read from the line itself, whatever the format: a JSON field (`level`, `lvl`, `severity`, ECS `log.level`; pino/bunyan and syslog numbers too), logfmt `level=`, or a level word near the start of text logs (`ERROR`, `WARN:`, `[error]`), plus the klog (`E1018 …`, Kubernetes) and Android (`E/Tag:`) one-letter forms. A line without one (a stack trace, a continued message) takes the level of the line before it, so a trace stays with its error. Lines with no level at all are always shown: omnitail does not hide what it cannot classify.

## Keys

| Key | Action |
|---|---|
| `h` / `F1` | keys of the current view (`Tab`: all keys, `Esc` closes it) |
| `q` / `Esc` | quit (or go back from a popup or the scrollback; `Esc` after splitting panes goes back to the first screen) |
| `Ctrl+C` | quit from anywhere |
| `N` | clear all panes (the history is kept) |
| `n` | choose one pane to clear |
| `b` | choose a pane and browse its whole history, including what was cleared |
| `B` | browse all panes' histories merged, in arrival order |
| `/` | highlight a pattern in all panes, live (empty to remove) |
| `E` / `e` | filter all panes / one pane: `error` shows matching lines, `!info` hides them (empty to remove) |
| `L` / `l` | levels shown in all panes / one pane: all, debug+, info+, warn+, error (see [Levels](#levels)) |
| `P` / `p` | pause/resume all panes / one pane (lines keep arriving in the history) |
| `C` / `c` | omnitail's coloring on/off for all panes / one pane |
| `s` | choose a pane's color scheme in a list you can filter by typing (`auto`, `none`, or any scheme) |
| `w` | wrap long lines at word boundaries, or cut them (`›`) |
| `Enter` / `Backspace` | split a k8s pane into its pods, then a pod into its containers / back up one level (`Esc`: back to the first screen) |

Uppercase keys act on all panes; lowercase ones open a picker (`1`-`9` or arrows and `Enter`).

Patterns are regular expressions with smart case: case-insensitive unless they contain an uppercase letter. The prompt keeps a history (`↑`/`↓`).

In the history views (`b`, `B`), a cursor selects a line: `j`/`k`, `PgUp`/`PgDn`, `g`/`G` (back to following the end), `←`/`→` when lines are cut. Once the cursor leaves the last line, the view stays put as lines arrive, and the title counts them (`↓ +12`). As in `less`, `/` and `?` search down and up (wrapping around), `n`/`N` go to the next match in the same/other direction, and `&pattern` shows only matching lines (`&!pattern` hides them). A pane's history starts with that pane's filter and level; `l` changes the level of the view only. `p` pauses the view (searches stay within what was there), `t` shows when each line arrived, and in a pane's history `c`/`s` toggle its colors and choose its scheme. `Enter` shows the selected line on its own, with JSON pretty-printed even after a prefix (`[pod/x] {…}`); `←`/`→` move to the previous/next line of the view. `y` copies the line to the clipboard: locally through `pbcopy`, `wl-copy`, `xclip` or `xsel`; over SSH through OSC 52, which the terminal must allow (iTerm2: Settings → General → Selection → "Applications in terminal may access clipboard").

## License

MIT, see `LICENSE`. The color schemes converted from multitail keep its MIT notice, in `THIRD_PARTY_LICENSES`.
