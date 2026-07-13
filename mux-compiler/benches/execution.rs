// Criterion benchmarks for end-to-end execution throughput of compiled Mux
// programs (runtime + linked stdlib), isolated from compile/link time.
//
// This is a CURATED workload set (benches/programs/*.mux), not the whole
// test_scripts corpus: some corpus programs are servers or need SQL/network
// services and would block a micro-bench, and trivial scripts are dominated by
// process-spawn overhead. The workloads here each run for ~tens of ms of real
// runtime work.
//
// Each workload is compiled ONCE to a native executable (untimed) via the `mux
// build` subcommand; the timed loop only spawns and runs that executable. Needs
// the runtime resolvable (sibling checkout or MUX_RUNTIME_LIB) and LLVM/clang,
// same as a normal `mux build`.
//
// Local/manual + non-blocking CI report; not a merge gate (see CONTRIBUTING.md).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use std::time::Duration;

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

struct Workload {
    name: String,
    exe: PathBuf,
}

fn programs_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("benches/programs")
}

// Compile every workload once to target/bench-exes/<name>. A workload that fails
// to compile fails the bench (see the assert below): these are curated programs
// that must build wherever the runtime + LLVM/clang are available.
static WORKLOADS: OnceLock<Vec<Workload>> = OnceLock::new();

fn workloads() -> &'static [Workload] {
    WORKLOADS.get_or_init(|| {
        let mux = env!("CARGO_BIN_EXE_mux");
        let out_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/bench-exes");
        if let Err(e) = std::fs::create_dir_all(&out_dir) {
            panic!("cannot create {}: {e}", out_dir.display());
        }

        let mut sources: Vec<PathBuf> = std::fs::read_dir(programs_dir())
            .expect("benches/programs should exist")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "mux"))
            .collect();
        sources.sort();

        let mut built = Vec::new();
        let mut failed = Vec::new();
        for src in sources {
            let name = src
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| src.to_string_lossy().into_owned());
            let exe = out_dir.join(&name);

            let status = Command::new(mux)
                .arg("build")
                .arg(&src)
                .arg("-o")
                .arg(&exe)
                .status();

            // Require both a success exit and an actual output file.
            match status {
                Ok(s) if s.success() && exe.is_file() => built.push(Workload { name, exe }),
                _ => failed.push(name),
            }
        }

        // Surface build failures loudly rather than silently reporting a partial
        // (or empty) set: a curated workload that will not compile is a real
        // problem worth failing the bench for (needs the runtime + LLVM/clang).
        assert!(
            failed.is_empty(),
            "execution workloads failed to compile: {failed:?} \
             (is the runtime + LLVM/clang available?)"
        );
        assert!(
            !built.is_empty(),
            "no execution workloads found under benches/programs"
        );
        built
    })
}

fn run(exe: &Path) {
    let status = Command::new(exe)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("workload executable should spawn");
    assert!(status.success(), "workload executable exited non-zero");
}

fn bench_execution(c: &mut Criterion) {
    let mut group = c.benchmark_group("execution");
    for w in workloads() {
        group.bench_with_input(BenchmarkId::from_parameter(&w.name), w, |b, w| {
            b.iter(|| run(&w.exe));
        });
    }
    group.finish();
}

fn configured() -> Criterion {
    // Each workload is a whole compiled process running tens of ms, so keep the
    // sample count small; the report is non-blocking anyway.
    Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(2))
}

criterion_group!(
    name = benches;
    config = configured();
    targets = bench_execution
);
criterion_main!(benches);
