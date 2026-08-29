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
`./scripts/dev-cargo.sh test --all-features`, and the generated-program suite
before committing. Run strict rustdoc and the full fixture/snapshot checks for
parser, code-generation, or diagnostic changes.

## Documentation

See [`README.md`](README.md), [`docs/`](docs/), and the cross-repository
decisions in `mux-context`.
