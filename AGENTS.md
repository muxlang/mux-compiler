# mux-compiler

`mux-compiler` is the Mux language compiler and CLI. It parses source,
performs semantic analysis, emits LLVM IR, and links generated programs with
`mux-runtime`.

Cross-repository architecture and release facts live in
[`mux-context`](https://github.com/muxlang/mux-context). Read its canonical
[`SKILL.md`](https://github.com/muxlang/mux-context/blob/main/SKILL.md) before
changing syntax, diagnostics, generated code, or the runtime ABI.

## Invariants

- Keep generated LLVM ownership operations aligned with
  `mux-context/docs/design/memory.md` and the runtime ABI.
- Parser, lexer, diagnostic, and snapshot changes must include focused fixtures
  and preserve deterministic output.
- Do not hand-edit insta snapshots; regenerate them with the documented test
  command and review every resulting change.
- The released compiler/runtime version contract is recorded in `CHANGELOG.md`
  and the lockfile; update both when that contract changes.

## Quality gate

Run `cargo fmt --all -- --check`,
`./scripts/dev-cargo.sh clippy --all-targets --all-features -- -D warnings`,
and the focused tests before committing. The pre-commit hook runs these checks
and skips only `test_executable_all_mux_files_in_dir`, the multi-minute run over
the full executable fixture corpus. The corpus compiles and runs up to two
non-network fixtures at once. Network fixtures run serially, and snapshots
are checked in sorted order. `MUX_EXECUTABLE_TEST_JOBS` overrides the default
of at most two workers. For example, set it to eight on a larger machine:

```bash
MUX_EXECUTABLE_TEST_JOBS=8 ./scripts/dev-cargo.sh test -p mux-lang --test executable_integration
```

CI runs that corpus, including the leak-check-runtime pass. Run it locally
when changing executable behavior or for ad-hoc verification after executable
changes or when investigating a failure:

```bash
./scripts/dev-cargo.sh test -p mux-lang --test executable_integration
```

For the complete local test and snapshot check, run
`./scripts/run-checks.sh --reject-unreferenced-snapshots`. Run strict rustdoc
and the generated-program suite for parser, code-generation, or diagnostic
changes.

## Documentation

See [`README.md`](README.md), [`docs/`](docs/), and the cross-repository
decisions in `mux-context`. See the
[cross-repo testing policy](https://github.com/muxlang/mux-context/blob/main/docs/testing-policy.md)
for the pre-commit and CI split.
