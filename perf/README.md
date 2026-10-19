# Performance

Two layers, both replayable at will and run by CI (`.github/workflows/perf.yml`):

1. **Checks inside the app** (`src/perf.rs`): ingest, rendering, history, k8s splits and file loading,
   timed without a terminal. Each one prints a `PERF` line and fails when it misses its budget.
   Budgets leave room for slow CI runners: they catch regressions, not small drifts.

   ```sh
   cargo test --release perf -- --ignored --nocapture --test-threads=1 | grep -o 'PERF.*'
   ```

2. **End to end** (`perf/run.sh`): the release binary in a pseudo-terminal against big generated
   logs, measured with `/usr/bin/time` (memory, CPU) and omnitail's own frame statistics (logged on
   exit with `OMNITAIL_LOG=omnitail::perf=debug`). Writes `target/perf/report.md`.

   ```sh
   perf/run.sh                                  # 1M lines, streams at 10k and 50k lines/s
   LINES=200000 RATES=10000 perf/run.sh         # quicker
   ```

`perf/gen-logs.py` writes the logs: a deterministic mix of JSON, logfmt, nginx access logs, text with
levels, ANSI-colored lines, Java stack traces and a few very long JSON lines. `static` writes a file,
`stream` appends at a given rate.

Measure release builds only: debug builds are 5 to 10 times slower.

## Checks and budgets

| Check | Budget |
|---|---|
| level detection | ≥ 300k lines/s |
| ingest, one pane | ≥ 100k lines/s |
| ingest, k8s source split in pods | ≥ 50k lines/s |
| frame, 4 panes of 50k lines, wrapped | ≤ 16 ms |
| frame, pane filter keeping almost nothing (looks through all history) | ≤ 50 ms |
| frame, history with 100k new lines below the cursor | ≤ 16 ms |
| history search / view filter over 100k lines | ≤ 100 ms |
| merged history (`B`) of 2 × 100k lines | ≤ 200 ms |
| k8s split into pods, then a pod into containers, 100k lines | ≤ 100 ms |
| file of 1M lines opened with `--tail 100000` | ≤ 1 s |

## Reference results

MacBook (Apple silicon), release build, 2026-10-19.

| Check | Result |
|---|---|
| level detection | 2.9M lines/s |
| ingest, one pane | 1.8M lines/s |
| ingest, k8s split in pods | 1.25M lines/s |
| frame, 4 panes wrapped | 0.4 ms |
| frame, filter keeping ~0 | 7.2 ms |
| frame, 100k new below cursor | 1.0 ms |
| history search, 100k | 7.8 ms |
| history filter, 100k | 0.5 ms |
| merged history (`B`), 2 × 100k | 2.1 ms |
| split into pods / a pod into containers | 11 ms / 3.6 ms |
| file tail 100k of 1M at start | 38 ms |

| End to end | Result | First lines on screen | Frame p50 / p99 | Max memory | CPU |
|---|---|---|---|---|---|
| `--tail 1000000` on a 135 MB file | all loaded in 1.7 s | 0.65 s | 0.9 / 4.9 ms | 465 MB | 2.0 s |
| 1M lines piped on stdin | 1.04M lines/s | 11 ms | 0.8 / 1.3 ms | 36 MB | 2.2 s |
| file growing 10k lines/s for 10 s | all 99,900 shown | — | 0.8 / 1.5 ms | 34 MB | 1% |
| file growing 50k lines/s for 10 s | all 499,500 shown | — | 0.8 / 1.6 ms | 47 MB | 7% |

Notes: each line costs about 120 bytes of bookkeeping plus its text, so keeping a million lines takes
~450 MB; the 10,000 lines kept by default stay around 40 MB. For growing files, "first lines on
screen" only says when the generator started writing.
