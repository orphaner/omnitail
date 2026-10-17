#!/usr/bin/env python3
"""Converts multitail's color schemes into mtr's scheme format.

    scripts/convert-multitail.py path/to/multitail.conf > src/color/multitail.toml

multitail is MIT licensed (Copyright (c) 2023 Folkert van Heusden); the
output keeps that notice. What does not carry over is reported on stderr:
- value comparisons (cs_re_val_*, mcsre_val_*), which mtr does not support,
- alternating colors ("red|blue"): the first one is kept,
- mcsre* (merged attributes): treated like cs_re* (first rule wins).
Regexes are POSIX ERE; mtr's test suite compiles every converted one.
"""

import re
import sys

# Schemes for software that is long gone or very niche ("Divers").
EXCLUDED = {
    "asterisk", "nagtail", "vnetbr", "uucp", "inn", "nntpcache", "rsstail",
    "boinc", "boinctail", "lambdamoo", "motion", "ii",
}

ATTRIBUTES = {"bold": "bold", "blink": "bold", "inverse": "reverse", "reverse": "reverse", "underline": "underline"}

HEADER = """\
# Color schemes converted from multitail's multitail.conf by
# scripts/convert-multitail.py. Do not edit by hand: change the script.
#
# multitail - MIT License
# Copyright (c) 2023 Folkert van Heusden
# Permission is hereby granted, free of charge, to any person obtaining a copy
# of this software and associated documentation files (the "Software"), to deal
# in the Software without restriction, including without limitation the rights
# to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
# copies of the Software, and to permit persons to whom the Software is
# furnished to do so, subject to the following conditions:
# The above copyright notice and this permission notice shall be included in all
# copies or substantial portions of the Software.
# THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND. See
# THIRD_PARTY_LICENSES for the full text.
"""


def warn(msg):
    print(msg, file=sys.stderr)


def style(spec, where):
    """multitail "fg,bg,attr/attr|…" to mtr "fg bg:x attr"."""
    if "|" in spec:
        warn(f"{where}: alternating colors '{spec}', keeping the first")
    parts = (spec.split("|")[0].split(",") + ["", "", ""])[:3]
    fg, bg, attrs = (p.strip() for p in parts)
    words = []
    if fg:
        words.append(fg)
    if bg:
        words.append(f"bg:{bg}")
    for attr in filter(None, attrs.split("/")):
        if attr not in ATTRIBUTES:
            warn(f"{where}: unknown attribute '{attr}', dropped")
            continue
        if ATTRIBUTES[attr] not in words:
            words.append(ATTRIBUTES[attr])
    return " ".join(words) or None


def groups(regex):
    """Number of capturing groups, for cs_re_s (color each group)."""
    try:
        return re.compile(regex).groups
    except re.error:
        # Count unescaped "(" outside bracket expressions.
        count, i, in_class = 0, 0, False
        while i < len(regex):
            c = regex[i]
            if c == "\\":
                i += 2
                continue
            if in_class:
                in_class = c != "]"
            elif c == "[":
                in_class = True
            elif c == "(":
                count += 1
            i += 1
        return count


def toml_string(s):
    """A TOML literal string, multi-line form when the text has a quote."""
    if "'" not in s:
        return f"'{s}'"
    if "'''" in s:
        raise ValueError(f"cannot quote {s!r}")
    return f"'''{s}'''"


def main(path):
    schemes = {}  # name -> {"description", "rules", "paths"}
    order = []
    current = None
    skipped_values = 0
    with open(path, encoding="utf-8", errors="replace") as f:
        for n, line in enumerate(f, 1):
            line = line.rstrip("\n")
            where = f"{path}:{n}"
            if line.startswith("colorscheme:"):
                _, name, *rest = line.split(":", 2)
                current = name.strip()
                if current in EXCLUDED:
                    current = None
                    continue
                schemes[current] = {"description": (rest[0] if rest else "").strip(), "rules": [], "paths": []}
                order.append(current)
            elif line.startswith("scheme:"):
                _, name, regex = line.split(":", 2)
                if name in schemes:
                    schemes[name]["paths"].append(regex)
            elif re.match(r"^(cs_re|mcsre)\w*:", line):
                kind, rest = line.split(":", 1)
                if current is None:
                    continue
                if "_val_" in kind:
                    skipped_values += 1
                    warn(f"{where}: {kind} (value comparison) not supported, skipped")
                    continue
                color, regex = rest.split(":", 1)
                st = style(color, where)
                if st is None:
                    warn(f"{where}: no style in '{color}', skipped")
                    continue
                if kind.endswith("_s"):
                    count = groups(regex)
                    if count == 0:
                        warn(f"{where}: {kind} without a group, coloring the whole match")
                        schemes[current]["rules"].append((regex, 0, st))
                    for g in range(1, count + 1):
                        schemes[current]["rules"].append((regex, g, st))
                else:
                    schemes[current]["rules"].append((regex, 0, st))

    out = [HEADER]
    for name in order:
        s = schemes[name]
        if not s["rules"]:
            warn(f"scheme {name}: no rules left, dropped")
            continue
        out.append(f"\n[scheme.{name}]")
        if s["description"]:
            out.append(f"description = {toml_string(s['description'])}")
        if s["paths"]:
            out.append("paths = [" + ", ".join(toml_string(p) for p in s["paths"]) + "]")
        out.append("rules = [")
        for regex, group, st in s["rules"]:
            g = f", group = {group}" if group else ""
            out.append(f"  {{ regex = {toml_string(regex)}{g}, style = \"{st}\" }},")
        out.append("]")
    print("\n".join(out))
    kept = sum(1 for n in order if schemes[n]["rules"])
    warn(f"{kept} schemes, {sum(len(schemes[n]['rules']) for n in order)} rules; "
         f"{len(EXCLUDED)} schemes excluded; {skipped_values} value rules skipped")


if __name__ == "__main__":
    main(sys.argv[1] if len(sys.argv) > 1 else "multitail.conf")
