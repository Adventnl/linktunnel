#!/usr/bin/env python3
"""Compare two nexus-bench reports and fail if a gated p95 regressed by more than 10 %.

    bench/compare.py BASE.json HEAD.json [--tolerance 0.10] [--slack-ms 1.5]

A metric fails only if it is both more than `tolerance` slower *and* more than `slack-ms`
slower in absolute terms, so sub-millisecond jitter on a fast metric cannot trip the gate.
Scenarios missing from either report are skipped (e.g. no Xvfb on the runner).
"""
import argparse
import json
import sys


def metrics(report):
    out = {}
    for name in ("glass", "x11"):
        block = report.get(name)
        if block:
            out[f"{name}.p95_ms"] = block["p95_ms"]
            out[f"{name}.p50_ms"] = block["p50_ms"]
    shaped = report.get("shaped")
    if shaped:
        out["shaped.adaptive.p95_ms"] = shaped["adaptive"]["p95_ms"]
    for row in report.get("codec", []):
        if row["resolution"] in ("1280x720", "1920x1080"):
            out[f"codec.{row['resolution']}.encode_p95_ms"] = row["encode_p95_ms"]
            out[f"codec.{row['resolution']}.decode_convert_p50_ms"] = row["decode_convert_p50_ms"]
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("base")
    ap.add_argument("head")
    ap.add_argument("--tolerance", type=float, default=0.10)
    ap.add_argument("--slack-ms", type=float, default=1.5)
    args = ap.parse_args()

    base, head = metrics(json.load(open(args.base))), metrics(json.load(open(args.head)))
    failed = False
    print(f"{'metric':44} {'base':>10} {'head':>10} {'change':>9}")
    for key in sorted(base.keys() & head.keys()):
        b, h = base[key], head[key]
        change = (h - b) / b if b else 0.0
        regressed = change > args.tolerance and (h - b) > args.slack_ms
        failed |= regressed
        print(f"{key:44} {b:10.2f} {h:10.2f} {change:+8.1%}{'  <-- REGRESSION' if regressed else ''}")
    for key in sorted((base.keys() ^ head.keys())):
        print(f"{key:44} (only in one report; skipped)")
    if failed:
        print(f"\nFAIL: at least one metric regressed by more than {args.tolerance:.0%} (and {args.slack_ms} ms).")
        return 1
    print("\nOK: no gated metric regressed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
