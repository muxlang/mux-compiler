// Criterion benchmarks for the compiler pipeline phases (lex, parse, semantics,
// codegen) plus an end-to-end lex-to-IR pipeline.
//
// Unlike a representative-trio bench, this runs each phase over the WHOLE
// compiling corpus: every `*.mux` directly under `test_scripts/` (error_cases/
// excluded, since those intentionally fail to compile). Each file is one
// criterion benchmark inside its phase group, so criterion writes a per-file
// median to target/criterion/<phase>/<file>/new/estimates.json. scripts/
// bench-report.py aggregates those medians into a box-and-whisker per phase.
//
// These are a local/manual dev tool and a non-blocking CI report; they are not a
// merge gate (see CONTRIBUTING.md).

use std::cell::RefCell;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::OnceLock;
use std::time::Duration;

use criterion::{
    black_box, criterion_group, criterion_main, BatchSize, BenchmarkId, Criterion,
};
use inkwell::context::Context;
use mux_lang::ast::AstNode;
use mux_lang::codegen::CodeGenerator;
use mux_lang::diagnostic::Files;
use mux_lang::lexer::{Lexer, Token};
use mux_lang::module_resolver::ModuleResolver;
use mux_lang::parser::Parser;
use mux_lang::semantics::SemanticAnalyzer;
use mux_lang::source::Source;

struct Program {
    /// Benchmark id (file stem, e.g. "arithmetic").
    name: String,
    /// Absolute path to the source file (used as the resolver/diagnostics anchor).
    path: PathBuf,
    src: String,
}

fn test_scripts_dir() -> PathBuf {
    // CARGO_MANIFEST_DIR is the crate dir; test_scripts lives one level up.
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../test_scripts")
}

fn lex(src: &str) -> Vec<Token> {
    let mut source = Source::from_string(src.to_string());
    let mut lexer = Lexer::new(&mut source);
    lexer.lex_all().unwrap_or_default()
}

fn parse(src: &str) -> Option<Vec<AstNode>> {
    let tokens = lex(src);
    let mut parser = Parser::new(&tokens);
    parser.parse().ok()
}

// A fresh analyzer + diagnostics registry for a program, wired with a module
// resolver anchored at the file's directory so imports resolve exactly as they
// do in `run_compile` (src/main.rs).
fn fresh(prog: &Program) -> (SemanticAnalyzer, Files) {
    let base = prog
        .path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let resolver = Rc::new(RefCell::new(ModuleResolver::new(base)));
    let mut files = Files::new();
    files.add(&prog.path, prog.src.clone());
    (SemanticAnalyzer::new_with_resolver(resolver), files)
}

// The corpus is discovered and validated once. A file is kept only if it fully
// lexes, parses, and passes semantics, so "all compiling programs" is literally
// true and no phase panics on a bad input.
static CORPUS: OnceLock<Vec<Program>> = OnceLock::new();

fn corpus() -> &'static [Program] {
    CORPUS.get_or_init(|| {
        let dir = test_scripts_dir();
        let mut entries: Vec<PathBuf> = fs::read_dir(&dir)
            .unwrap_or_else(|e| panic!("cannot read {}: {e}", dir.display()))
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|ext| ext == "mux"))
            .collect();
        entries.sort();

        let mut kept = Vec::new();
        let mut skipped = 0usize;
        for path in entries {
            let Ok(src) = fs::read_to_string(&path) else {
                skipped += 1;
                continue;
            };
            let name = path
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            let prog = Program { name, path, src };

            let compiles = parse(&prog.src)
                .map(|nodes| {
                    let (mut analyzer, mut files) = fresh(&prog);
                    analyzer.analyze(&nodes, Some(&mut files)).is_empty()
                })
                .unwrap_or(false);

            if compiles {
                kept.push(prog);
            } else {
                skipped += 1;
            }
        }

        eprintln!(
            "compile bench corpus: {} programs ({} skipped as non-compiling)",
            kept.len(),
            skipped
        );
        kept
    })
}

fn bench_lex(c: &mut Criterion) {
    let mut group = c.benchmark_group("lex");
    for prog in corpus() {
        group.bench_with_input(BenchmarkId::from_parameter(&prog.name), prog, |b, prog| {
            b.iter(|| lex(black_box(&prog.src)));
        });
    }
    group.finish();
}

fn bench_parse(c: &mut Criterion) {
    let mut group = c.benchmark_group("parse");
    for prog in corpus() {
        let tokens = lex(&prog.src);
        group.bench_with_input(BenchmarkId::from_parameter(&prog.name), prog, |b, _| {
            b.iter(|| {
                let mut parser = Parser::new(black_box(&tokens));
                let _ = parser.parse();
            });
        });
    }
    group.finish();
}

fn bench_semantics(c: &mut Criterion) {
    let mut group = c.benchmark_group("semantics");
    for prog in corpus() {
        let Some(nodes) = parse(&prog.src) else {
            continue;
        };
        group.bench_with_input(BenchmarkId::from_parameter(&prog.name), prog, |b, prog| {
            b.iter_batched(
                || fresh(prog),
                |(mut analyzer, mut files)| {
                    let _ = analyzer.analyze(black_box(&nodes), Some(&mut files));
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_codegen(c: &mut Criterion) {
    let mut group = c.benchmark_group("codegen");
    for prog in corpus() {
        let Some(nodes) = parse(&prog.src) else {
            continue;
        };
        group.bench_with_input(BenchmarkId::from_parameter(&prog.name), prog, |b, prog| {
            b.iter_batched(
                || {
                    // Setup (untimed): a fully analyzed analyzer ready for codegen.
                    let (mut analyzer, mut files) = fresh(prog);
                    let _ = analyzer.analyze(&nodes, Some(&mut files));
                    analyzer
                },
                |mut analyzer| {
                    let context = Context::create();
                    let mut codegen = CodeGenerator::new(&context, &mut analyzer, &prog.name);
                    let _ = codegen.generate(black_box(&nodes));
                },
                BatchSize::SmallInput,
            );
        });
    }
    group.finish();
}

fn bench_pipeline(c: &mut Criterion) {
    let mut group = c.benchmark_group("pipeline");
    for prog in corpus() {
        group.bench_with_input(BenchmarkId::from_parameter(&prog.name), prog, |b, prog| {
            b.iter(|| {
                let Some(nodes) = parse(black_box(&prog.src)) else {
                    return;
                };
                let (mut analyzer, mut files) = fresh(prog);
                let _ = analyzer.analyze(&nodes, Some(&mut files));
                let context = Context::create();
                let mut codegen = CodeGenerator::new(&context, &mut analyzer, &prog.name);
                let _ = codegen.generate(&nodes);
            });
        });
    }
    group.finish();
}

fn configured() -> Criterion {
    // ~300 benchmarks (corpus x phases); keep a full run in the minutes range.
    // It is non-blocking, and a single phase can be run with `cargo bench -- lex`.
    Criterion::default()
        .sample_size(20)
        .warm_up_time(Duration::from_millis(300))
        .measurement_time(Duration::from_secs(1))
}

criterion_group!(
    name = benches;
    config = configured();
    targets = bench_lex, bench_parse, bench_semantics, bench_codegen, bench_pipeline
);
criterion_main!(benches);
