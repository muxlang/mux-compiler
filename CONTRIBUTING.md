# Contributing to Mux

Thanks for your interest! This guide explains how to contribute to Mux.

---

## How to Get Started

1. Fork the repository.
2. Clone your fork locally.
3. Install Rust (edition 2024 required): `rustup install stable`
4. Run the bootstrap script to install LLVM 22 automatically:

   ```bash
   ./scripts/bootstrap-dev.sh
   ```

   This script detects your OS and installs LLVM 22 and clang. It supports:
   - Arch Linux (via yay)
   - Debian/Ubuntu (via apt)
   - macOS (via Homebrew)

5. Build the compiler using the dev wrapper:

   ```bash
   ./scripts/dev-cargo.sh build
   ```

   The `dev-cargo.sh` script automatically sets the correct LLVM environment variables.

6. Run tests to make sure everything is working:

   ```bash
   ./scripts/run-checks.sh
   ```

7. Running Mux programs during development:

    ```bash
    cargo run -- run test_scripts/your_file.mux
    ```

    The workspace defaults to the compiler crate, so this also works from the repo root.

8. Run the Docker-backed integration suite when touching networking or external SQL providers:

   ```bash
   ./scripts/integration-checks.sh
   ```

9. Run the benchmarks when investigating compile or runtime performance (local and
   non-blocking CI; never a merge gate):

   ```bash
   ./scripts/dev-cargo.sh bench       # phases over the compiling corpus + execution workloads
   python3 scripts/bench-report.py    # box-and-whisker per phase -> target/bench-report/
   ```

   Compare against a saved baseline with `cargo bench -- --save-baseline main`, then re-run
   with `--baseline main`.

10. Check for memory leaks and invalid accesses with Valgrind (requires `valgrind` installed):

    ```bash
    ./scripts/valgrind-checks.sh            # both legs
    ./scripts/valgrind-checks.sh --programs # Leg A only (compiled programs)
    ./scripts/valgrind-checks.sh --compiler # Leg B only (the compiler)
    ```

    Leg A compiles every `test_scripts/*.mux` program and runs the resulting
    binary under Valgrind; definitely-lost and indirectly-lost leaks and memory
    errors fail (still-reachable is ignored). This mirrors the PR-blocking CI
    job. Leg B runs the compiler itself under Valgrind and is report-only,
    filtering statically linked LLVM noise via `infra/valgrind-llvm.supp`.

11. Profile the compiler and runtime with external tools (see the Setup documentation for platform-specific guidance).

12. Always run `cargo fmt` and `cargo clippy` before committing changes.
13. For releases, bump `version` in `mux-compiler/Cargo.toml`, add the matching section in `CHANGELOG.md`, and update the README version badge (see the Release Process in AGENTS.md).
14. Create a new branch for your changes, named with the tag first, and description after, e.g., `bug/xyz-fix` or `feature/new-feat`.
15. Make your changes.
16. Run tests again to ensure nothing is broken.
17. Commit your changes with clear messages.
18. Push your branch to your fork.
19. Open a Pull Request against the `main` branch of the original repository.
20. AI agents should follow the guidelines in [AGENTS.md](AGENTS.md).

---

## Working with mux-runtime

Compiled Mux programs link against
[mux-runtime](https://github.com/muxlang/mux-runtime), which lives in its own
repository. **You do not need to clone it.** It is a git dependency on that
repo's `main` branch, so `cargo build` fetches it, and `Cargo.lock` pins one
exact commit - which is why `--locked` builds are reproducible.

### Landing a coupled change

When a compiler change needs a runtime change (a new FFI symbol, say), the
runtime side merges first. After it does, move the pin:

```bash
cargo update -p mux-runtime
```

Commit the resulting `Cargo.lock` alongside your compiler change. Without it,
CI builds `--locked` against the old commit and your change looks broken for no
visible reason.

Nothing advances the pin on a schedule, and nothing needs to: it moves when a
change needs it to move, and a release can move it deliberately. Meanwhile CI
already tests against runtime `main` on both sides - this repo's `build.yml`
checks out the runtime's `main` source, and mux-runtime's CI builds this repo's
`main` against it - so a runtime change that breaks the compiler surfaces
without waiting for the pin to advance.

### Where the runtime library comes from

The compiler never builds a runtime while compiling your program. It links a
prebuilt library, found in this order:

1. `MUX_RUNTIME_LIB` - a path to a built library (a `.a` file, not a directory).
2. A library beside the compiler binary, or in `../lib` next to it. This is what
   a release install ships.
3. The library in `target/`, built from the commit `Cargo.lock` pins. This is
   the one you get while developing - but **`cargo build` alone does not produce
   it**. Cargo emits a dependency's rlib and never its staticlib, so you need:

   ```bash
   cargo build -p mux-runtime
   ```

   `scripts/run-checks.sh` and `scripts/valgrind-checks.sh` already do this. If
   compiled programs suddenly fail to link with undefined references to
   `mux_*` symbols, this is why.

That library always carries mux-runtime's `full` feature set. Linking the full
runtime costs nothing: static linking pulls in only the archive members a
program actually references, so a program that never touches `sql` does not
carry SQLite.

The one thing to watch is rule 1. A leftover `MUX_RUNTIME_LIB` - in your
environment or in a gitignored `.cargo/config.toml` - wins over the library
cargo just built, so runtime behavior stops matching the source you are reading.
Check it first when something inexplicable happens.

To test a compiler change against uncommitted runtime changes, build the runtime
in your checkout and point `MUX_RUNTIME_LIB` at the resulting `.a`.

`mux version` prints the runtime it resolved, including the locked commit
(`runtime v0.5.0+g1a2b3c4`). Include that line in bug reports.

---

## What Contributions Are Welcome

- Bug fixes
- Documentation improvements
- Design discussions
- Compiler frontend improvements
- Playground components

We generally don't accept contributions that:
- Break core language design
- Add unnecessary complexity
- Go against project goals (see README for design philosophy)

---

## Code Style

- Rust code should follow `rustfmt` defaults (edition 2024).
- Use `clippy` to catch common mistakes.
- Write clear, concise comments where necessary.
- Ensure all new code is covered by tests.
- Snapshot testing uses [insta](https://insta.rs/) - run `cargo insta test` to review snapshots.
- I don't care what your commit messages look like, as long as they are clear about what changed.

---

## Tooling

- Standard Rust tooling: `cargo fmt`, `cargo clippy`, and `cargo test`.
- Canonical local verification: `./scripts/run-checks.sh`
- Docker-backed integration verification: `./scripts/integration-checks.sh`
- Valgrind memory checking: `./scripts/valgrind-checks.sh` (Leg A programs are PR-blocking; Leg B compiler run is report-only)
- Benchmarks: `cargo bench` (criterion) plus `scripts/bench-report.py` for the per-phase box-and-whisker; local/manual and non-blocking CI, never a merge gate
- Compiler profiling: external tools only
- Runtime profiling: external tools only

### Generated program campaigns

The [issue #386](https://github.com/muxlang/mux-compiler/issues/386) generator
composes bindings, blocks, closures, references, collections, optionals,
results, tuples, generics, enum payloads, loop exits, and early returns into
deterministic Mux programs. Build the compiler, then run a campaign:

```bash
cargo build
scripts/run-generated-programs.py --count 100 --start-seed 1
```

Each seed reproduces exactly. Rerun one failure and keep its source and output:

```bash
scripts/run-generated-programs.py --seed 1143 --keep-failures /tmp/mux-failures
```

The runner rejects compile errors, crashes, timeouts, incomplete output, and
wrong answers. The generator writes a `.json` manifest beside each `.mux` file;
the runner uses it to require the seed's final oracle, compare oracle output
against the exact value the generator intended, and fail multi-seed campaigns
that miss a core feature bucket.

Pass an `rc-leak-check` runtime archive with `--runtime-lib PATH` to make live
reference-counted blocks fail too. Add `--check-ir-determinism` when you want
the campaign to build each generated program twice with `--intermediate` and
compare the emitted LLVM IR. That mode is intentionally opt-in because generic
method emission has a known nondeterministic ordering gap tracked separately.

---

## Issue & PR Guidelines

- Check for existing issues before opening a new one.
- Use the provided issue templates for bugs and features.
- Link your PR to an existing issue when possible.
- Respond to review comments promptly and respectfully.
- Add the @muxlang/maintainers team as a reviewer to your PRs.

---

## Project Resources

- **Language Spec**: [README.md](README.md)
- **AI Agent Workflow**: [AGENTS.md](AGENTS.md)
- **License**: MIT

---
