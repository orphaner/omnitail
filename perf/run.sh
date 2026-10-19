#!/bin/sh
# omnitail end to end: the release binary in a pseudo-terminal, against big
# generated logs. Writes target/perf/report.md (see perf/README.md).
#
#   perf/run.sh            # all scenarios
#   LINES=200000 perf/run.sh   # smaller file, e.g. in CI
set -eu

cd "$(dirname "$0")/.."
OUT=target/perf
LINES=${LINES:-1000000}
RATES=${RATES:-"10000 50000"}
mkdir -p "$OUT"

cargo build --release --quiet
BIN=$PWD/target/release/omnitail

if [ ! -f "$OUT/big-$LINES.log" ]; then
    echo "generating $LINES lines…"
    python3 perf/gen-logs.py static --lines "$LINES" --out "$OUT/big-$LINES.log"
fi
BIG=$PWD/$OUT/big-$LINES.log

# run NAME SECONDS COMMAND: runs COMMAND in a 200×60 pty for SECONDS, then
# presses q. Leaves omnitail's log in $OUT/NAME.log and time's in $OUT/NAME.time.
run() {
    name=$1 seconds=$2
    shift 2
    log=$PWD/$OUT/$name.log
    # LC_ALL=C: time's numbers with a dot, whatever the locale.
    cmd="stty rows 60 cols 200; OMNITAIL_LOG=info,omnitail::perf=debug LC_ALL=C exec $TIME $BIN --log-file $log $*"
    if [ "$(uname)" = Darwin ]; then
        (sleep "$seconds"; printf q) | script -q /dev/null sh -c "$cmd 2>$OUT/$name.time" >/dev/null
    else
        (sleep "$seconds"; printf q) | script -qec "$cmd 2>$OUT/$name.time" /dev/null >/dev/null
    fi
}

if [ "$(uname)" = Darwin ]; then TIME="/usr/bin/time -l"; else TIME="/usr/bin/time -v"; fi

echo "startup: --tail $LINES"
run startup 8 --tail "$LINES" "$BIG"

echo "stdin: $LINES lines piped in"
run stdin 8 - "<" "$BIG"

for rate in $RATES; do
    echo "stream: $rate lines/s for 10 s"
    live=$PWD/$OUT/live-$rate.log
    : >"$live"
    python3 perf/gen-logs.py stream --rate "$rate" --seconds 10 --out "$live" 2>"$OUT/stream-$rate.written" &
    run "stream-$rate" 12 "$live"
    wait
done

python3 -I - "$OUT" "$LINES" $RATES <<'EOF'
import re, sys
from datetime import datetime
out, lines, rates = sys.argv[1], int(sys.argv[2]), sys.argv[3:]

def stamp(line):
    return datetime.fromisoformat(line.split()[0].replace("Z", "+00:00"))

def scenario(name):
    text = open(f"{out}/{name}.log").read().splitlines()
    summary = next(l for l in text if "summary" in l and "omnitail::perf" in l)
    fields = dict(re.findall(r"(\w+)=(\d+)", summary))
    start = stamp(summary).timestamp() - int(fields["running_ms"]) / 1000
    batches = [(stamp(l).timestamp() - start, int(re.search(r"lines=(\d+)", l)[1])) for l in text if " batch " in l or l.endswith("batch")]
    timing = open(f"{out}/{name}.time").read()
    rss = re.search(r"(\d+)\s+maximum resident set size", timing)
    rss_mb = int(rss[1]) / 2**20 if rss else int(re.search(r"Maximum resident set size \(kbytes\): (\d+)", timing)[1]) / 1024
    cpu = sum(float(x) for x in re.findall(r"([\d.]+)\s+(?:user|sys)\b", timing)) or sum(
        float(x) for x in re.findall(r"(?:User|System) time \(seconds\): ([\d.]+)", timing))
    return fields, batches, rss_mb, cpu

def reached(batches, n):
    return next((t for t, got in batches if got >= n), None)

rows = []
f, b, rss, cpu = scenario("startup")
t = reached(b, lines)
rows.append((f"startup, --tail {lines:,}", f"{t * 1000:.0f} ms to load all" if t else "not all loaded", f"{f['first_lines_ms']} ms", f"{int(f['frame_p50_us'])/1000:.1f} / {int(f['frame_p99_us'])/1000:.1f} ms", f"{rss:.0f} MB", f"{cpu:.1f} s"))
f, b, rss, cpu = scenario("stdin")
t = reached(b, lines)
rows.append((f"stdin, {lines:,} lines", f"{lines / t / 1000:.0f}k lines/s" if t else f"{f['lines']} lines only", f"{f['first_lines_ms']} ms", f"{int(f['frame_p50_us'])/1000:.1f} / {int(f['frame_p99_us'])/1000:.1f} ms", f"{rss:.0f} MB", f"{cpu:.1f} s"))
for rate in rates:
    f, b, rss, cpu = scenario(f"stream-{rate}")
    written = int(open(f"{out}/stream-{rate}.written").read().strip() or 0)
    rows.append((f"file growing {int(rate):,} lines/s, 10 s", f"{f['lines']}/{written} lines shown", f"{f['first_lines_ms']} ms", f"{int(f['frame_p50_us'])/1000:.1f} / {int(f['frame_p99_us'])/1000:.1f} ms", f"{rss:.0f} MB", f"{cpu:.1f} s ({cpu / 12 * 100:.0f}%)"))

header = ("scenario", "result", "first lines on screen", "frame p50 / p99", "max memory", "CPU time (user + sys)")
table = ["| " + " | ".join(header) + " |", "|" + "---|" * len(header)]
table += ["| " + " | ".join(r) + " |" for r in rows]
report = "# omnitail end to end\n\n" + "\n".join(table) + "\n"
open(f"{out}/report.md", "w").write(report)
print(report)
EOF
