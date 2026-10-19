#!/usr/bin/env python3
"""Realistic log lines for omnitail's performance runs (see perf/README.md).

    gen-logs.py static --lines 1000000 --out big.log
    gen-logs.py stream --rate 10000 --seconds 10 --out live.log   # appends

The mix: JSON (pino/ECS-like), logfmt, nginx access logs, text with levels,
lines colored by their source (ANSI), Java stack traces over several lines
and a few very long JSON lines. The same seed gives the same file, so runs
can be compared.
"""

import argparse
import json
import random
import sys
import time

LEVELS = ["debug", "info", "info", "info", "info", "warn", "error"]
PATHS = ["/api/items", "/api/orders", "/api/users", "/health", "/static/app.js"]
WORDS = "order invoice payment user cart session cache queue job retry timeout".split()


def stamp(rng):
    return f"2026-10-19T{rng.randrange(24):02}:{rng.randrange(60):02}:{rng.randrange(60):02}.{rng.randrange(1000):03}Z"


def json_line(rng):
    return json.dumps({
        "time": stamp(rng), "level": rng.choice(LEVELS), "msg": f"{rng.choice(WORDS)} {rng.choice(WORDS)}",
        "user_id": rng.randrange(100000), "duration_ms": rng.randrange(2000), "ok": rng.random() > 0.1,
        "trace_id": f"{rng.getrandbits(64):016x}",
    }, separators=(",", ":"))


def logfmt_line(rng):
    return (f'time={stamp(rng)} level={rng.choice(LEVELS)} msg="{rng.choice(WORDS)} processed" '
            f"job={rng.randrange(100000)} attempt={rng.randrange(1, 4)} duration={rng.randrange(900)}ms")


def nginx_line(rng):
    status = rng.choice([200, 200, 200, 201, 204, 301, 304, 404, 500, 502])
    return (f'10.{rng.randrange(256)}.{rng.randrange(256)}.{rng.randrange(256)} - - '
            f'[19/Oct/2026:{rng.randrange(24):02}:{rng.randrange(60):02}:{rng.randrange(60):02} +0000] '
            f'"{rng.choice(["GET", "GET", "POST", "PUT", "DELETE"])} {rng.choice(PATHS)}/{rng.randrange(1000)} HTTP/1.1" '
            f'{status} {rng.randrange(50000)} "-" "Mozilla/5.0 (X11; Linux x86_64)"')


def text_line(rng):
    level = rng.choice(LEVELS).upper()
    return (f"2026-10-19 {rng.randrange(24):02}:{rng.randrange(60):02}:{rng.randrange(60):02}.{rng.randrange(1000):03} "
            f"{level:5} [worker-{rng.randrange(16)}] c.e.shop.{rng.choice(WORDS).title()}Service - "
            f"{rng.choice(WORDS)} {rng.randrange(10000)} handled in {rng.randrange(500)}ms")


def ansi_line(rng):
    color = rng.choice([31, 32, 33, 34, 36])
    return f"\x1b[{color}m{rng.choice(LEVELS).upper()}\x1b[0m {rng.choice(WORDS)} checkpoint {rng.randrange(100000)}"


def stack_trace(rng):
    lines = [f"2026-10-19 12:00:00.000 ERROR [worker-1] c.e.shop.OrderService - order {rng.randrange(10000)} failed",
             "java.lang.IllegalStateException: payment declined"]
    for i in range(rng.randrange(10, 30)):
        lines.append(f"\tat com.example.shop.{rng.choice(WORDS).title()}Service.handle({rng.choice(WORDS).title()}Service.java:{rng.randrange(1, 900)})")
    lines.append("Caused by: java.net.SocketTimeoutException: Read timed out")
    lines.append("\t... 42 more")
    return lines


def long_line(rng):
    items = [{"id": i, "name": rng.choice(WORDS), "price": rng.randrange(10000) / 100} for i in range(rng.randrange(100, 1000))]
    return json.dumps({"time": stamp(rng), "level": "debug", "msg": "cart dump", "items": items}, separators=(",", ":"))


KINDS = [(json_line, 30), (logfmt_line, 20), (nginx_line, 20), (text_line, 15), (ansi_line, 5),
         (stack_trace, 1), (long_line, 0.2)]


def lines(rng):
    """Endless log lines; a stack trace counts as several."""
    makers, weights = zip(*KINDS)
    while True:
        made = rng.choices(makers, weights)[0](rng)
        yield from made if isinstance(made, list) else [made]


def static(args):
    rng = random.Random(args.seed)
    with open(args.out, "w") as out:
        for _, line in zip(range(args.lines), lines(rng)):
            out.write(line + "\n")


def stream(args):
    """Appends `rate` lines per second for `seconds`, in small chunks."""
    rng = random.Random(args.seed)
    source = lines(rng)
    tick = 0.01
    per_tick = max(1, round(args.rate * tick))
    start = time.monotonic()
    written = 0
    with open(args.out, "a") as out:
        while (elapsed := time.monotonic() - start) < args.seconds:
            due = int(elapsed / tick) * per_tick
            chunk = [next(source) for _ in range(max(0, due - written))]
            if chunk:
                out.write("\n".join(chunk) + "\n")
                out.flush()
                written += len(chunk)
            time.sleep(tick)
    print(written, file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--seed", type=int, default=42)
    sub = parser.add_subparsers(dest="mode", required=True)
    s = sub.add_parser("static", help="write a file of --lines lines")
    s.add_argument("--lines", type=int, required=True)
    s.add_argument("--out", required=True)
    s.set_defaults(run=static)
    s = sub.add_parser("stream", help="append --rate lines per second for --seconds (prints how many)")
    s.add_argument("--rate", type=int, required=True)
    s.add_argument("--seconds", type=float, required=True)
    s.add_argument("--out", required=True)
    s.set_defaults(run=stream)
    args = parser.parse_args()
    args.run(args)


if __name__ == "__main__":
    main()
