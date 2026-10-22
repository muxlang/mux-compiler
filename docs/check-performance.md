# Check command measurements

Measured on 2026-10-22 with the same release compiler for `check` and `build`.
These measurements used the frontend-only `feat/check-command` implementation
before integration with `feat/self-update`. They do not measure the combined
compiler's updater initialization or installation locking overhead.
The three existing fixtures cover arithmetic, class fields, and generic bounds.
Each command had two warmup runs, followed by ten measured runs per fixture.
Measured order alternated between check/build and build/check. Times include
process startup and use Python's `time.perf_counter()`. Rust compilation and
runtime setup were completed before measurement.

| Fixture | Check median (ms) | Build median (ms) | Build/check |
| --- | ---: | ---: | ---: |
| `arithmetic.mux` | 8.74 | 250.38 | 28.64 |
| `class_scalar_fields.mux` | 9.54 | 252.80 | 26.51 |
| `generic_bounds.mux` | 9.41 | 255.22 | 27.13 |

Check samples ranged from 7.07 to 10.69 ms; build samples ranged from 239.30
to 269.96 ms. This measures frontend validation against native compilation and
linking, with the release runtime archive already available. It does not measure
program execution or a cold installation.

The machine had an Intel Core i9-11900H at 2.50 GHz with 16 logical CPUs,
running Linux 7.2.11-arch1-1 on x86_64 with glibc 2.44. Toolchain versions were
Rust 1.93.1, LLVM 22.1.8, and clang 23.1.3. The Cargo release profile used thin
LTO and one codegen unit. The release build and this change's test runs had
finished before sampling. This was a shared development machine without CPU
pinning or frequency control; other workloads were not controlled. These
wall-clock results describe these small fixtures on this machine. Larger import graphs and backend work can change the ratio.

## Reproduce

Run from the compiler repository root. Build the compiler and static runtime
explicitly, then run the sampling script:

```bash
CI=1 CARGO_TARGET_DIR="$PWD/target" ./scripts/dev-cargo.sh build \
  --locked --release -p mux-lang -p mux-runtime
python3 - <<'PY'
import os
import pathlib
import statistics
import subprocess
import tempfile
import time

root = pathlib.Path.cwd()
compiler = root / "target/release/mux"
env = dict(os.environ, MUX_RUNTIME_LIB=str(root / "target/release/libmux_runtime.a"), NO_COLOR="1")
with tempfile.TemporaryDirectory(prefix="mux-check-bench-") as temporary:
    for fixture in ["arithmetic.mux", "class_scalar_fields.mux", "generic_bounds.mux"]:
        source = root / "test_scripts" / fixture
        commands = {
            "check": [str(compiler), "check", str(source)],
            "build": [str(compiler), "build", str(source), "-o", str(pathlib.Path(temporary) / fixture.removesuffix(".mux"))],
        }
        def run(name):
            start = time.perf_counter()
            subprocess.run(commands[name], env=env, capture_output=True, check=True, timeout=60)
            return (time.perf_counter() - start) * 1000
        for _ in range(2):
            for name in commands:
                run(name)
        samples = {name: [] for name in commands}
        for sample in range(10):
            for name in (["check", "build"] if sample % 2 == 0 else ["build", "check"]):
                samples[name].append(run(name))
        print(fixture, {name: statistics.median(values) for name, values in samples.items()})
PY
```

Build output paths are inside a temporary directory and are removed afterward.
Neither command requests intermediate IR. `check` writes no build artifacts.
