#!/usr/bin/env python3
"""A/B benchmark regression gate for a PR head binary against a baseline
binary (plan.md step 5), following roc-lang/roc's threshold rule rather
than a blunt ratio:

- Dual threshold: only trips if BOTH the relative change exceeds
  --pct-threshold AND the absolute change exceeds --abs-ms-threshold, so a
  large relative swing on an already-tiny program cannot fire alone.
- Confirmation re-run: a trip re-runs hyperfine for that file only and
  fails only if the second run also exceeds both thresholds.
- Byte-identical-output escape hatch: if `mux build --intermediate`
  produces the same LLVM IR text from both binaries for a file, any timing
  difference on that file is definitionally measurement noise (codegen did
  not change what it emits), so a trip is recorded but does not fail the
  gate.

Not a CI-only tool: run locally with two binary directories to reproduce a
failure exactly.

Known limitation (mux-compiler#344): codegen's emission order for
monomorphized generic methods is not deterministic between two runs of the
SAME binary on the SAME file (confirmed: 4 different sha256 hashes across 4
back-to-back `mux build --intermediate` runs of a generics-heavy file). The
IR-diff escape hatch above can therefore read "differs" on a
generics-heavy file even when nothing real changed. This does not produce
false failures - it just means those files fall back to relying on the
dual threshold + confirmation re-run alone, the same as any file where the
escape hatch cannot prove identity. Fix the root cause in codegen, not here.
"""
import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

PCT_THRESHOLD_DEFAULT = 4.0
ABS_MS_THRESHOLD_DEFAULT = 5.0


def confined(candidate: Path, repo_root: Path, what: str) -> Path:
    # Resolve and require the path stay within the repo, so untrusted CLI
    # arguments cannot read or write outside the project tree. Same pattern
    # as scripts/bench-report.py.
    resolved = candidate.resolve()
    if resolved != repo_root and repo_root not in resolved.parents:
        print(f"error: {what} {resolved} is outside {repo_root}", file=sys.stderr)
        raise SystemExit(2)
    return resolved


def load_curated_list(list_path: Path, repo_root: Path) -> list[Path]:
    programs = []
    for line in list_path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        programs.append(confined(repo_root / line, repo_root, f"curated program {line!r}"))
    if not programs:
        print(f"error: no programs listed in {list_path}", file=sys.stderr)
        raise SystemExit(2)
    return programs


def build_ir(mux_bin: Path, runtime_lib: Path, program: Path, workdir: Path) -> str:
    """Compile `program` with --intermediate in an isolated workdir (so
    baseline and PR builds, which both write the .ll beside the source
    file, never collide) and return the resulting IR text."""
    local_src = workdir / program.name
    local_src.write_text(program.read_text(encoding="utf-8"), encoding="utf-8")
    env = {**os.environ, "MUX_RUNTIME_LIB": str(runtime_lib)}
    result = subprocess.run(
        [str(mux_bin), "build", "--intermediate", str(local_src), "-o", str(workdir / "out")],
        env=env,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        print(
            f"error: `mux build --intermediate` failed for {program}:\n{result.stderr}",
            file=sys.stderr,
        )
        raise SystemExit(1)
    ir_path = local_src.with_suffix(".ll")
    return ir_path.read_text(encoding="utf-8")


def run_hyperfine(
    baseline_bin: Path,
    baseline_runtime_lib: Path,
    pr_bin: Path,
    pr_runtime_lib: Path,
    program: Path,
    export_json: Path,
    workdir: Path,
) -> dict:
    # `mux run` with no -o writes a default-named executable beside the
    # source file. Without -o here, dozens of hyperfine iterations against
    # the real checkout would leave stray build artifacts in the working
    # tree (and, in the worst case, let a later run silently execute a
    # stale binary if a compile step were ever skipped).
    baseline_env = f"MUX_RUNTIME_LIB={baseline_runtime_lib}"
    pr_env = f"MUX_RUNTIME_LIB={pr_runtime_lib}"
    cmd = [
        "hyperfine",
        "--warmup",
        "1",
        "--min-runs",
        "3",
        "--export-json",
        str(export_json),
        "--command-name",
        "baseline",
        f"{baseline_env} {baseline_bin} run {program} -o {workdir / 'baseline-out'}",
        "--command-name",
        "pr",
        f"{pr_env} {pr_bin} run {program} -o {workdir / 'pr-out'}",
    ]
    result = subprocess.run(cmd, capture_output=True, text=True)
    if result.returncode != 0:
        print(f"error: hyperfine failed for {program}:\n{result.stderr}", file=sys.stderr)
        raise SystemExit(1)
    data = json.loads(export_json.read_text(encoding="utf-8"))
    by_name = {r["command"]: r for r in data["results"]}
    return by_name


def evaluate(
    by_name: dict, pct_threshold: float, abs_ms_threshold: float
) -> tuple[bool, float, float]:
    baseline_median = by_name["baseline"]["median"]
    pr_median = by_name["pr"]["median"]
    pct_change = (pr_median - baseline_median) / baseline_median * 100
    abs_delta_ms = abs(pr_median - baseline_median) * 1000
    tripped = pct_change > pct_threshold and abs_delta_ms > abs_ms_threshold
    return tripped, pct_change, abs_delta_ms


def evaluate_program(
    program: Path,
    baseline_bin: Path,
    baseline_runtime_lib: Path,
    pr_bin: Path,
    pr_runtime_lib: Path,
    tmp_path: Path,
    pct_threshold: float,
    abs_ms_threshold: float,
) -> dict:
    name = program.name
    baseline_ir_dir = tmp_path / f"{name}-baseline"
    pr_ir_dir = tmp_path / f"{name}-pr"
    baseline_ir_dir.mkdir()
    pr_ir_dir.mkdir()
    baseline_ir = build_ir(baseline_bin, baseline_runtime_lib, program, baseline_ir_dir)
    pr_ir = build_ir(pr_bin, pr_runtime_lib, program, pr_ir_dir)
    ir_identical = baseline_ir == pr_ir

    export_json = tmp_path / f"{name}.hyperfine.json"
    hf_workdir = tmp_path / f"{name}-hf"
    hf_workdir.mkdir()
    by_name = run_hyperfine(
        baseline_bin, baseline_runtime_lib, pr_bin, pr_runtime_lib, program, export_json, hf_workdir
    )
    tripped, pct_change, abs_delta_ms = evaluate(by_name, pct_threshold, abs_ms_threshold)

    verdict = "ok"
    if tripped:
        if ir_identical:
            verdict = "escaped (IR identical)"
        else:
            # Confirmation re-run: only this file, only once.
            confirm_json = tmp_path / f"{name}.hyperfine.confirm.json"
            confirm_by_name = run_hyperfine(
                baseline_bin, baseline_runtime_lib, pr_bin, pr_runtime_lib, program, confirm_json, hf_workdir
            )
            confirm_tripped, _, _ = evaluate(confirm_by_name, pct_threshold, abs_ms_threshold)
            verdict = "REGRESSION" if confirm_tripped else "ok (confirmation run did not reproduce)"

    return {
        "program": name,
        "baseline_ms": by_name["baseline"]["median"] * 1000,
        "pr_ms": by_name["pr"]["median"] * 1000,
        "pct_change": pct_change,
        "abs_delta_ms": abs_delta_ms,
        "ir_identical": ir_identical,
        "verdict": verdict,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--baseline-bin-dir", required=True, type=Path, help="dir containing baseline mux + libmux_runtime.a")
    parser.add_argument("--pr-bin-dir", required=True, type=Path, help="dir containing PR-head mux + libmux_runtime.a")
    parser.add_argument("--curated-list", type=Path, help="default: scripts/ci/ab-curated-programs.txt")
    parser.add_argument("--pct-threshold", type=float, default=PCT_THRESHOLD_DEFAULT)
    parser.add_argument("--abs-ms-threshold", type=float, default=ABS_MS_THRESHOLD_DEFAULT)
    args = parser.parse_args()

    repo_root = Path(__file__).resolve().parent.parent.parent
    curated_list = confined(
        args.curated_list or (repo_root / "scripts" / "ci" / "ab-curated-programs.txt"),
        repo_root,
        "curated list",
    )
    programs = load_curated_list(curated_list, repo_root)

    baseline_bin_dir = args.baseline_bin_dir.resolve()
    pr_bin_dir = args.pr_bin_dir.resolve()
    baseline_bin = baseline_bin_dir / "mux"
    pr_bin = pr_bin_dir / "mux"
    baseline_runtime_lib = baseline_bin_dir / "libmux_runtime.a"
    pr_runtime_lib = pr_bin_dir / "libmux_runtime.a"
    for p in (baseline_bin, pr_bin, baseline_runtime_lib, pr_runtime_lib):
        if not p.is_file():
            print(f"error: expected file not found: {p}", file=sys.stderr)
            return 2

    rows = []
    with tempfile.TemporaryDirectory(prefix="ab-bench-gate-") as tmp:
        tmp_path = Path(tmp)
        for program in programs:
            rows.append(
                evaluate_program(
                    program,
                    baseline_bin,
                    baseline_runtime_lib,
                    pr_bin,
                    pr_runtime_lib,
                    tmp_path,
                    args.pct_threshold,
                    args.abs_ms_threshold,
                )
            )
    failed = any(r["verdict"] == "REGRESSION" for r in rows)

    header = f"{'program':<28} {'baseline (ms)':>13} {'pr (ms)':>10} {'change':>8} {'abs (ms)':>9} {'ir':>11}  verdict"
    print(header)
    print("-" * len(header))
    for r in rows:
        print(
            f"{r['program']:<28} {r['baseline_ms']:>13.2f} {r['pr_ms']:>10.2f} "
            f"{r['pct_change']:>+7.1f}% {r['abs_delta_ms']:>9.2f} "
            f"{'identical' if r['ir_identical'] else 'differs':>11}  {r['verdict']}"
        )

    if failed:
        print("\n::error::One or more programs regressed beyond the dual threshold, confirmed on re-run.")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
