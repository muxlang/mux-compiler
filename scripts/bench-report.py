#!/usr/bin/env python3
"""Render a per-phase box-and-whisker of compiler benchmark timings.

Criterion measures each `test_scripts/*.mux` file individually and writes a
median to `target/criterion/<phase>/<file>/new/estimates.json`. This script
aggregates those per-file medians into one box (min / Q1 / median / Q3 / max +
1.5*IQR outliers) per phase and emits a standalone HTML page with inline SVG.

This is a reporting/visualization tool - it is NOT a CI gate and does not fail on
regressions. Run it after `cargo bench`:

    python3 scripts/bench-report.py [CRITERION_DIR] [-o OUTPUT.html]

CRITERION_DIR defaults to `<repo>/target/criterion`.
"""

import argparse
import json
import math
import statistics
import sys
from pathlib import Path

# Phase groups in pipeline order; any other group found (e.g. execution) is
# appended after these.
#
# `--summary-json` feeds the PR-comment workflow, which has its own whitelist of
# phase labels (BENCH_FAST_PHASES / BENCH_SLOW_PHASES in
# .github/workflows/pr-comment.yml) and drops anything not on it. A phase added
# here shows up in the JSON and this script's own report, but stays out of the PR
# charts until it is added there too.
PREFERRED_ORDER = ["lex", "parse", "semantics", "codegen", "pipeline", "execution"]


def collect(criterion_dir: Path) -> dict[str, list[float]]:
    """Map each benchmark group to the list of per-file median times (ns)."""
    groups: dict[str, list[float]] = {}
    for estimates in criterion_dir.glob("*/*/new/estimates.json"):
        group = estimates.parts[-4]
        if group == "report":
            continue
        try:
            data = json.loads(estimates.read_text(encoding="utf-8"))
            median = float(data["median"]["point_estimate"])
        except (OSError, ValueError, KeyError):
            continue
        groups.setdefault(group, []).append(median)
    return groups


def box_stats(values: list[float]) -> dict:
    """Tukey box-and-whisker summary for a list of samples."""
    ordered = sorted(values)
    if len(ordered) == 1:
        v = ordered[0]
        return {"q1": v, "q2": v, "q3": v, "lo": v, "hi": v, "outliers": []}
    q1, q2, q3 = statistics.quantiles(ordered, n=4, method="inclusive")
    iqr = q3 - q1
    lo_fence, hi_fence = q1 - 1.5 * iqr, q3 + 1.5 * iqr
    inside = [v for v in ordered if lo_fence <= v <= hi_fence]
    outliers = [v for v in ordered if v < lo_fence or v > hi_fence]
    return {
        "q1": q1,
        "q2": q2,
        "q3": q3,
        "lo": min(inside) if inside else ordered[0],
        "hi": max(inside) if inside else ordered[-1],
        "outliers": outliers,
    }


def human_time(ns: float) -> str:
    for scale, unit in ((1e9, "s"), (1e6, "ms"), (1e3, "us"), (1.0, "ns")):
        if ns >= scale:
            return f"{ns / scale:.3g} {unit}"
    return f"{ns:.3g} ns"


def render_svg(order: list[str], stats: dict[str, dict], counts: dict[str, int]) -> str:
    # Log-scaled y-axis: phases differ by orders of magnitude.
    all_vals = [
        v
        for name in order
        for v in (
            stats[name]["lo"],
            stats[name]["hi"],
            *stats[name]["outliers"],
        )
        if v > 0
    ]
    if not all_vals:
        return "<p>No positive samples to plot.</p>"

    lo_exp = math.floor(math.log10(min(all_vals)))
    hi_exp = math.ceil(math.log10(max(all_vals)))
    if hi_exp == lo_exp:
        hi_exp += 1

    pad_l, pad_r, pad_t, pad_b = 70, 20, 20, 60
    col_w = 120
    plot_h = 360
    width = pad_l + pad_r + col_w * len(order)
    height = pad_t + pad_b + plot_h

    def y(ns: float) -> float:
        ns = max(ns, 10 ** lo_exp)
        frac = (math.log10(ns) - lo_exp) / (hi_exp - lo_exp)
        return pad_t + plot_h * (1 - frac)

    parts = [
        f'<svg viewBox="0 0 {width} {height}" width="{width}" height="{height}" '
        'role="img" font-family="ui-sans-serif, system-ui, sans-serif">'
    ]

    # Decade gridlines + labels.
    for exp in range(lo_exp, hi_exp + 1):
        gy = y(10 ** exp)
        parts.append(
            f'<line x1="{pad_l}" y1="{gy:.1f}" x2="{width - pad_r}" y2="{gy:.1f}" '
            'class="grid"/>'
        )
        parts.append(
            f'<text x="{pad_l - 8}" y="{gy + 4:.1f}" text-anchor="end" '
            f'class="tick">{human_time(10 ** exp)}</text>'
        )

    for i, name in enumerate(order):
        s = stats[name]
        cx = pad_l + col_w * i + col_w / 2
        half = 34
        y_q1, y_q2, y_q3 = y(s["q1"]), y(s["q2"]), y(s["q3"])
        y_lo, y_hi = y(s["lo"]), y(s["hi"])

        # Whisker line + caps.
        parts.append(
            f'<line x1="{cx}" y1="{y_hi:.1f}" x2="{cx}" y2="{y_lo:.1f}" class="whisk"/>'
        )
        for yy in (y_lo, y_hi):
            parts.append(
                f'<line x1="{cx - half / 2}" y1="{yy:.1f}" x2="{cx + half / 2}" '
                f'y2="{yy:.1f}" class="whisk"/>'
            )
        # Box + median.
        parts.append(
            f'<rect x="{cx - half}" y="{y_q3:.1f}" width="{2 * half}" '
            f'height="{max(y_q1 - y_q3, 1):.1f}" class="box"/>'
        )
        parts.append(
            f'<line x1="{cx - half}" y1="{y_q2:.1f}" x2="{cx + half}" '
            f'y2="{y_q2:.1f}" class="median"/>'
        )
        # Outliers.
        for o in s["outliers"]:
            parts.append(f'<circle cx="{cx}" cy="{y(o):.1f}" r="2.5" class="outlier"/>')
        # Labels.
        parts.append(
            f'<text x="{cx}" y="{height - pad_b + 20}" text-anchor="middle" '
            f'class="label">{name}</text>'
        )
        parts.append(
            f'<text x="{cx}" y="{height - pad_b + 36}" text-anchor="middle" '
            f'class="sub">n={counts[name]}, med {human_time(s["q2"])}</text>'
        )

    parts.append("</svg>")
    return "\n".join(parts)


HTML = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Mux compiler benchmarks - per-phase distribution</title>
<style>
  :root {{ color-scheme: light dark; }}
  body {{ font-family: ui-sans-serif, system-ui, sans-serif; margin: 2rem;
          background: #fff; color: #111; }}
  h1 {{ font-size: 1.25rem; }}
  p.meta {{ color: #666; }}
  .chart {{ overflow-x: auto; }}
  .grid {{ stroke: #ddd; stroke-width: 1; }}
  .tick, .sub {{ fill: #666; font-size: 11px; }}
  .label {{ fill: #111; font-size: 13px; font-weight: 600; }}
  .whisk {{ stroke: #555; stroke-width: 1.5; }}
  .box {{ fill: rgba(56,132,255,0.25); stroke: #3884ff; stroke-width: 1.5; }}
  .median {{ stroke: #d64545; stroke-width: 2; }}
  .outlier {{ fill: #d64545; opacity: 0.7; }}
  @media (prefers-color-scheme: dark) {{
    body {{ background: #111; color: #eee; }}
    p.meta {{ color: #999; }}
    .grid {{ stroke: #333; }}
    .tick, .sub {{ fill: #999; }}
    .label {{ fill: #eee; }}
    .whisk {{ stroke: #aaa; }}
  }}
</style>
</head>
<body>
<h1>Mux compiler benchmarks - per-phase distribution</h1>
<p class="meta">Each box aggregates the per-file median across the compiling
<code>test_scripts</code> corpus. Y-axis is log-scaled time. Non-blocking report.</p>
<div class="chart">{svg}</div>
</body>
</html>
"""


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("criterion_dir", nargs="?", type=Path)
    parser.add_argument("-o", "--output", type=Path)
    parser.add_argument(
        "--summary-json",
        type=Path,
        help="also write a compact {phases:[{name,median_ns,n}]} JSON here "
        "(consumed by the PR-comment workflow to render charts).",
    )
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent

    def confined(candidate: Path, what: str) -> Path:
        # Resolve and require the path stay within the repo, so untrusted CLI
        # arguments cannot read or write outside the project tree.
        resolved = candidate.resolve()
        if resolved != repo_root and repo_root not in resolved.parents:
            print(f"error: {what} {resolved} is outside {repo_root}", file=sys.stderr)
            raise SystemExit(2)
        return resolved

    criterion_dir = confined(
        args.criterion_dir or (repo_root / "target" / "criterion"), "criterion dir"
    )
    if not criterion_dir.is_dir():
        print(f"error: {criterion_dir} not found; run `cargo bench` first", file=sys.stderr)
        return 1

    groups = collect(criterion_dir)
    if not groups:
        print(f"error: no estimates found under {criterion_dir}", file=sys.stderr)
        return 1

    order = [g for g in PREFERRED_ORDER if g in groups]
    order += sorted(g for g in groups if g not in PREFERRED_ORDER)

    stats = {name: box_stats(groups[name]) for name in order}
    counts = {name: len(groups[name]) for name in order}
    svg = render_svg(order, stats, counts)

    output = confined(
        args.output or (repo_root / "target" / "bench-report" / "phase-distribution.html"),
        "output",
    )
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(HTML.format(svg=svg), encoding="utf-8")
    print(f"wrote {output}")

    if args.summary_json is not None:
        summary_json = confined(args.summary_json, "summary json")
        summary_json.parent.mkdir(parents=True, exist_ok=True)
        summary = {
            "phases": [
                {"name": name, "median_ns": stats[name]["q2"], "n": counts[name]}
                for name in order
            ]
        }
        summary_json.write_text(json.dumps(summary, indent=2), encoding="utf-8")
        print(f"wrote {summary_json}")

    for name in order:
        s = stats[name]
        print(
            f"  {name:<10} n={counts[name]:<3} "
            f"median={human_time(s['q2'])}  IQR=[{human_time(s['q1'])}, {human_time(s['q3'])}]"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
