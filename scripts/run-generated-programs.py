#!/usr/bin/env python3
"""Generate deterministic Mux programs and run the issue #386 oracles.

The compiler and program run as separate steps so failures retain a useful
category. Pass an rc-leak-check runtime with --runtime-lib to turn live blocks
at process exit into campaign failures.

Usage:
  scripts/run-generated-programs.py --count 100 --start-seed 1
  scripts/run-generated-programs.py --seed 1143 --keep-failures /tmp/mux-failures
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

ORACLE_RE = re.compile(r"^@@oracle:(?P<id>[^:]+):p(?P<side>[12]): (?P<value>.*)$")
CASE_STEM_RE = re.compile(r"^case_(?P<seed>\d{5,12})$")
MAX_SEED = 999_999_999_999
REPO_ROOT = Path(__file__).resolve().parent.parent
WORKSPACE_ROOT = REPO_ROOT.parent
SYSTEM_TEMP_ROOT = Path(tempfile.gettempdir()).resolve()
ALLOWED_PATH_ROOTS = (REPO_ROOT, WORKSPACE_ROOT, SYSTEM_TEMP_ROOT)
CORE_FEATURES = {
    "closure-capture",
    "collections",
    "enum-payload",
    "generic-int",
    "generic-string",
    "loop-break-continue",
    "optional-none",
    "optional-some",
    "reference-list-element",
    "result-ok-err",
    "tuple-field",
    "early-return-cleanup",
}


@dataclass(frozen=True)
class Failure:
    kind: str
    detail: str


@dataclass(frozen=True)
class ProgramManifest:
    seed: int
    features: set[str]
    feature_oracles: dict[str, str]
    expected_oracles: dict[str, str]
    required_oracles: set[str]


@dataclass
class OracleState:
    pending: dict[str, str]
    seen: set[str]
    completed_count: int = 0


def confined_path(candidate: Path, what: str) -> Path:
    resolved = candidate.resolve()
    if any(resolved == root or root in resolved.parents for root in ALLOWED_PATH_ROOTS):
        return resolved
    roots = ", ".join(str(root) for root in ALLOWED_PATH_ROOTS)
    raise ValueError(f"{what} {resolved} is outside allowed roots: {roots}")


def require_safe_seed(seed: int, label: str) -> int:
    if seed < 0 or seed > MAX_SEED:
        raise ValueError(f"{label} must be between 0 and {MAX_SEED}")
    return seed


def case_stem(seed: int) -> str:
    safe_seed = require_safe_seed(seed, "seed")
    stem = f"case_{safe_seed:05d}"
    if CASE_STEM_RE.fullmatch(stem) is None:
        raise ValueError(f"unsafe generated case name: {stem}")
    return stem


def generated_child(root: Path, seed: int, suffix: str = "") -> Path:
    child = root / f"{case_stem(seed)}{suffix}"
    resolved_root = root.resolve()
    resolved_child = child.resolve()
    if resolved_child != resolved_root and resolved_root not in resolved_child.parents:
        raise ValueError(f"generated path escaped work directory: {resolved_child}")
    return resolved_child


def safe_command_arg(arg: str) -> str:
    if "\x00" in arg:
        raise ValueError("command argument contains a NUL byte")
    if arg.startswith(("/", ".")):
        confined_path(Path(arg), "command argument")
    return arg


def safe_command(command: list[str]) -> list[str]:
    if not command:
        raise ValueError("empty command")
    executable = confined_path(Path(command[0]), "command executable")
    if not executable.is_file():
        raise ValueError(f"command executable not found: {executable}")
    return [str(executable), *(safe_command_arg(arg) for arg in command[1:])]


def load_manifest(path: Path) -> ProgramManifest | Failure:
    try:
        data = json.loads(path.read_text(encoding="utf-8"))
    except OSError as error:
        return Failure("manifest-error", f"could not read {path}: {error}")
    except json.JSONDecodeError as error:
        return Failure("manifest-error", f"could not parse {path}: {error}")

    try:
        seed = data["seed"]
        features = set(data["features"])
        feature_oracles = dict(data["feature_oracles"])
        expected_oracles = dict(data["expected_oracles"])
        required_oracles = set(data["required_oracles"])
    except (KeyError, TypeError) as error:
        return Failure("manifest-error", f"{path} has invalid manifest shape: {error}")

    if not isinstance(seed, int):
        return Failure("manifest-error", f"{path} seed must be an integer")
    if unknown := features - CORE_FEATURES:
        names = ", ".join(sorted(unknown))
        return Failure("manifest-error", f"{path} has unknown feature(s): {names}")
    if unknown := set(feature_oracles) - CORE_FEATURES:
        names = ", ".join(sorted(unknown))
        return Failure(
            "manifest-error", f"{path} has unknown feature oracle(s): {names}"
        )
    if missing := required_oracles - expected_oracles.keys():
        names = ", ".join(sorted(missing))
        return Failure("manifest-error", f"{path} requires unknown oracle(s): {names}")
    if missing := set(feature_oracles.values()) - required_oracles:
        names = ", ".join(sorted(missing))
        return Failure(
            "manifest-error", f"{path} has non-required feature oracle(s): {names}"
        )
    return ProgramManifest(
        seed, features, feature_oracles, expected_oracles, required_oracles
    )


def record_oracle_p1(state: OracleState, oracle_id: str, value: str) -> Failure | None:
    if oracle_id in state.pending:
        return Failure("wrong-answer", f"oracle {oracle_id} emitted p1 twice")
    state.pending[oracle_id] = value
    return None


def record_oracle_p2(
    state: OracleState,
    manifest: ProgramManifest | None,
    oracle_id: str,
    value: str,
) -> Failure | None:
    expected = state.pending.pop(oracle_id, None)
    if expected is None:
        return Failure("wrong-answer", f"oracle {oracle_id} emitted p2 before p1")
    if value != expected:
        return Failure(
            "wrong-answer",
            f"oracle {oracle_id} disagreed: p1={expected!r}, p2={value!r}",
        )
    if manifest is not None:
        manifest_expected = manifest.expected_oracles.get(oracle_id)
        if manifest_expected is not None and value != manifest_expected:
            return Failure(
                "wrong-answer",
                f"oracle {oracle_id} returned {value!r}; expected {manifest_expected!r}",
            )
    state.completed_count += 1
    state.seen.add(oracle_id)
    return None


def process_oracle_line(
    state: OracleState, manifest: ProgramManifest | None, line: str
) -> Failure | None:
    match = ORACLE_RE.match(line)
    if match is None:
        return None
    oracle_id = match.group("id")
    value = match.group("value")
    if match.group("side") == "1":
        return record_oracle_p1(state, oracle_id, value)
    return record_oracle_p2(state, manifest, oracle_id, value)


def validate_output(
    stdout: str, manifest: ProgramManifest | None = None
) -> Failure | None:
    state = OracleState(pending={}, seen=set())
    lines = stdout.splitlines()

    for line in lines:
        if failure := process_oracle_line(state, manifest, line):
            return failure

    if state.pending:
        ids = ", ".join(sorted(state.pending))
        return Failure("wrong-answer", f"missing p2 for oracle(s): {ids}")
    if state.completed_count == 0:
        return Failure("wrong-answer", "program emitted no differential oracle")
    if manifest is not None:
        missing_required = manifest.required_oracles - state.seen
        if missing_required:
            ids = ", ".join(sorted(missing_required))
            return Failure("wrong-answer", f"missing required oracle(s): {ids}")
    if not lines or lines[-1] != "done":
        return Failure(
            "incomplete-output", "program did not finish with the done marker"
        )
    return None


def classify_process_failure(returncode: int, stderr: str) -> Failure:
    if "still live at exit" in stderr:
        return Failure("leak", stderr.strip())
    if "internal compiler error" in stderr:
        return Failure("compiler-ice", stderr.strip())
    return Failure("nonzero-exit", f"exit {returncode}: {stderr.strip()}")


def run_command(
    command: list[str], env: dict[str, str], timeout: float
) -> subprocess.CompletedProcess[str] | Failure:
    try:
        checked_command = safe_command(command)
        return subprocess.run(
            checked_command,
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except subprocess.TimeoutExpired:
        return Failure("timeout", f"timed out after {timeout:g}s")
    except ValueError as error:
        return Failure("unsafe-command", str(error))
    except OSError as error:
        return Failure("spawn-error", str(error))


def build_ir(
    mux_bin: Path,
    program: Path,
    destination: Path,
    env: dict[str, str],
    timeout: float,
) -> str | Failure:
    ir_source = destination / program.name
    shutil.copy2(program, ir_source)
    executable = destination / "out"
    result = run_command(
        [
            str(mux_bin),
            "build",
            "--intermediate",
            str(ir_source),
            "-o",
            str(executable),
        ],
        env,
        timeout,
    )
    if isinstance(result, Failure):
        return Failure(f"ir-{result.kind}", result.detail)
    if result.returncode != 0:
        failure = classify_process_failure(result.returncode, result.stderr)
        if failure.kind == "nonzero-exit":
            failure = Failure("ir-compile-error", failure.detail)
        return failure
    ir_path = ir_source.with_suffix(".ll")
    try:
        return ir_path.read_text(encoding="utf-8")
    except OSError as error:
        return Failure("ir-missing", str(error))


def check_ir_determinism(
    mux_bin: Path,
    program: Path,
    work_dir: Path,
    env: dict[str, str],
    timeout: float,
) -> Failure | None:
    first_dir = work_dir / f"{program.stem}-ir-a"
    second_dir = work_dir / f"{program.stem}-ir-b"
    first_dir.mkdir()
    second_dir.mkdir()
    first_ir = build_ir(mux_bin, program, first_dir, env, timeout)
    if isinstance(first_ir, Failure):
        return first_ir
    second_ir = build_ir(mux_bin, program, second_dir, env, timeout)
    if isinstance(second_ir, Failure):
        return second_ir
    if first_ir != second_ir:
        return Failure(
            "ir-nondeterminism",
            "two --intermediate builds from the same source emitted different IR",
        )
    return None


def preserve_failure(
    destination: Path,
    program: Path,
    failure: Failure,
    stdout: str,
    stderr: str,
) -> None:
    destination = confined_path(destination, "failure artifact directory")
    destination.mkdir(parents=True, exist_ok=True)
    stem = program.stem
    shutil.copy2(program, destination / program.name)
    (destination / f"{stem}.failure.txt").write_text(
        f"kind: {failure.kind}\ndetail: {failure.detail}\n",
        encoding="utf-8",
    )
    (destination / f"{stem}.stdout.txt").write_text(stdout, encoding="utf-8")
    (destination / f"{stem}.stderr.txt").write_text(stderr, encoding="utf-8")


def run_program(
    mux_bin: Path,
    program: Path,
    executable: Path,
    env: dict[str, str],
    timeout: float,
    manifest: ProgramManifest | None,
    *,
    check_ir: bool,
    work_dir: Path,
) -> tuple[Failure | None, str, str]:
    if check_ir:
        ir_failure = check_ir_determinism(mux_bin, program, work_dir, env, timeout)
        if ir_failure is not None:
            return ir_failure, "", ""

    build = run_command(
        [str(mux_bin), "build", str(program), "-o", str(executable)], env, timeout
    )
    if isinstance(build, Failure):
        return Failure(f"compile-{build.kind}", build.detail), "", ""
    if build.returncode != 0:
        failure = classify_process_failure(build.returncode, build.stderr)
        if failure.kind == "nonzero-exit":
            failure = Failure("compile-error", failure.detail)
        return failure, build.stdout, build.stderr

    run = run_command([str(executable)], env, timeout)
    if isinstance(run, Failure):
        return Failure(f"run-{run.kind}", run.detail), "", ""
    if run.returncode != 0:
        return (
            classify_process_failure(run.returncode, run.stderr),
            run.stdout,
            run.stderr,
        )
    return validate_output(run.stdout, manifest), run.stdout, run.stderr


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--mux", type=Path, default=REPO_ROOT / "target/debug/mux")
    parser.add_argument("--runtime-lib", type=Path)
    parser.add_argument("--count", type=int, default=100)
    parser.add_argument("--start-seed", type=int, default=1)
    parser.add_argument(
        "--seed", type=int, help="run exactly one seed; overrides the seed range"
    )
    parser.add_argument("--timeout", type=float, default=30.0)
    parser.add_argument("--keep-failures", type=Path)
    parser.add_argument(
        "--check-ir-determinism",
        action="store_true",
        help="build each generated program twice with --intermediate and compare IR",
    )
    parser.add_argument(
        "--allow-partial-coverage",
        action="store_true",
        help="do not fail when a multi-seed campaign misses a core feature bucket",
    )
    parser.add_argument("--verbose", action="store_true")
    args = parser.parse_args()
    if args.count < 1:
        parser.error("--count must be at least 1")
    try:
        require_safe_seed(args.start_seed, "--start-seed")
        if args.seed is not None:
            require_safe_seed(args.seed, "--seed")
        require_safe_seed(args.start_seed + args.count - 1, "last generated seed")
    except ValueError as error:
        parser.error(str(error))
    if args.timeout <= 0:
        parser.error("--timeout must be greater than zero")
    return args


def resolve_mux_binary(candidate: Path) -> Path | Failure:
    try:
        mux_bin = confined_path(candidate, "mux binary")
    except ValueError as error:
        return Failure("path-error", str(error))
    if not mux_bin.is_file():
        return Failure(
            "path-error", f"mux binary not found at {mux_bin}; run cargo build first"
        )
    return mux_bin


def runtime_env(args: argparse.Namespace) -> dict[str, str] | Failure:
    env = dict(os.environ)
    if args.runtime_lib is not None:
        try:
            runtime_lib = confined_path(args.runtime_lib, "runtime library")
        except ValueError as error:
            return Failure("path-error", str(error))
        if not runtime_lib.is_file():
            return Failure("path-error", f"runtime library not found at {runtime_lib}")
        env["MUX_RUNTIME_LIB"] = str(runtime_lib)
    return env


def selected_seeds(args: argparse.Namespace) -> list[int] | range:
    if args.seed is not None:
        return [args.seed]
    return range(args.start_seed, args.start_seed + args.count)


def generate_programs(args: argparse.Namespace, programs_dir: Path) -> int:
    generator_command = [
        sys.executable,
        str(REPO_ROOT / "scripts/generate-programs.py"),
        "--out",
        str(programs_dir),
    ]
    if args.seed is not None:
        generator_command.extend(["--seed", str(args.seed)])
    else:
        generator_command.extend(
            ["--count", str(args.count), "--start-seed", str(args.start_seed)]
        )
    generated = subprocess.run(
        generator_command, capture_output=True, text=True, check=False
    )
    if generated.returncode != 0:
        print(generated.stderr, file=sys.stderr, end="")
    return generated.returncode


def load_seed_manifest(program: Path, seed: int) -> ProgramManifest | Failure:
    manifest_path = program.with_suffix(".json")
    manifest = load_manifest(manifest_path)
    if isinstance(manifest, Failure):
        return manifest
    if manifest.seed != seed:
        return Failure(
            "manifest-error",
            f"{manifest_path} seed {manifest.seed} does not match case {seed}",
        )
    return manifest


def run_seed(
    args: argparse.Namespace,
    mux_bin: Path,
    env: dict[str, str],
    work_dir: Path,
    programs_dir: Path,
    seed: int,
) -> tuple[Failure | None, set[str]]:
    try:
        program = generated_child(programs_dir, seed, ".mux")
        executable = generated_child(work_dir, seed)
    except ValueError as error:
        return Failure("path-error", str(error)), set()

    manifest = load_seed_manifest(program, seed)
    if isinstance(manifest, Failure):
        return manifest, set()

    failure, stdout, stderr = run_program(
        mux_bin,
        program,
        executable,
        env,
        args.timeout,
        manifest,
        check_ir=args.check_ir_determinism,
        work_dir=work_dir,
    )
    if failure is not None and args.keep_failures is not None:
        preserve_failure(args.keep_failures, program, failure, stdout, stderr)
    return failure, set(manifest.feature_oracles)


def validate_campaign_coverage(
    args: argparse.Namespace, campaign_features: set[str]
) -> bool:
    if args.seed is not None or args.allow_partial_coverage:
        return True
    missing_features = CORE_FEATURES - campaign_features
    if not missing_features:
        return True
    names = ", ".join(sorted(missing_features))
    print(f"FAIL campaign: missing generated feature coverage: {names}")
    return False


def main() -> int:
    args = parse_args()
    mux_bin = resolve_mux_binary(args.mux)
    if isinstance(mux_bin, Failure):
        print(f"error: {mux_bin.detail}", file=sys.stderr)
        return 2

    env = runtime_env(args)
    if isinstance(env, Failure):
        print(f"error: {env.detail}", file=sys.stderr)
        return 2

    seeds = selected_seeds(args)
    failures: list[tuple[int, Failure]] = []
    campaign_features: set[str] = set()
    with tempfile.TemporaryDirectory(prefix="mux-generated-") as tmp:
        work_dir = Path(tmp)
        programs_dir = work_dir / "programs"
        if generate_programs(args, programs_dir) != 0:
            return 2

        for seed in seeds:
            failure, seed_features = run_seed(
                args, mux_bin, env, work_dir, programs_dir, seed
            )
            campaign_features.update(seed_features)
            if failure is None:
                if args.verbose:
                    print(f"PASS seed {seed}")
                continue
            failures.append((seed, failure))
            print(f"FAIL seed {seed}: {failure.kind}: {failure.detail}")

    total = 1 if args.seed is not None else args.count
    if not validate_campaign_coverage(args, campaign_features):
        return 1
    if args.verbose:
        names = ", ".join(sorted(campaign_features))
        print(f"covered generated features: {names}")
    print(f"generated campaign: {total - len(failures)}/{total} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
