# Mux Compiler: AI Agent Guidelines

Please also read the 10x Dev Skill that covers best practices at ~/.opencode/skills/derek-10x-dev-practices/SKILL.md if present.

> Cross-repo architecture, design rationale, the feature map, and the release
> process live in [muxlang/mux-context](https://github.com/muxlang/mux-context).

## Critical Rules
- **No hacks or workarounds** - write clean, production‑ready code. No hardcoding, no temporary solutions, no fighting the type system. If something is hard, ask for clarification, then do it the right way.
- **No special characters** - avoid em‑dashes, emojis, or other non‑ASCII characters in code, comments, or commit messages.
- **Understand existing code first** - read relevant modules before implementing anything new. Follow existing patterns.
- **Ask when unsure** - converse with the user to clarify requirements. If still unsure, explore the codebase and propose a solution.
- **Small, tested edits** - build and test frequently during development.
- **Follow Rust best practices** - idiomatic code, proper error handling, clear naming.
- **Never touch git** - do not run git commands, create commits, or modify git config. Let the user handle version control.
- **If confused about language design**, check README.md first, then stop and ask for clarification.
- **No clippy warnings** - code must pass `cargo clippy --all-targets --all-features -- -D warnings` (run this exact command for strict linting).
- **Pre-existing Issues** - if you encounter an issue that seems to be pre‑existing, _ALWAYS_ bring it up that you uncovered it, and then proceed with fixes.
- **Remove outdated comments** - ensure comments reflect current code.
- **NEVER READ THE `.env` FILE** - do not read or parse the `.env` file. If environment variables are needed, ask the user to provide them explicitly.
- When running commands that require environment variables, execute `source .env` in the shell to load them. This keeps secrets in process memory without exposing file contents to the AI context.

## Critical Understanding
Mux is a statically‑typed, reference‑counted language that aims for clean, zero‑cost abstractions. The compiler generates LLVM IR and links with a C/Rust runtime. The goal is a modern, easy‑to‑learn language with strong static typing.

## Development Process

### Quick Reference Commands
```bash
# Build the project
cargo build

# Test a Mux file (primary test method)
cargo run -- run test_scripts/test_file.mux

# Check for errors (no build)
cargo check

# Format code
cargo fmt

# Run clippy (no errors allowed)
cargo clippy
```

### Testing Approach

All of the following tools should be installed, if you find an issue running these commands, please stop and ask the user for help.

Only run the following commands after you have completed your code changes and are ready to test, do not run them during development, as they can be time‑consuming.

When feature complete and ready for testing a feature:
1. Run `cargo build` to verify compilation.
2. Run `cargo run -- test_scripts/test_file.mux` to test functionality.
   - Create test files that cover the new feature.
   - Add them to existing test files if appropriate, or remove ad‑hoc tests.
3. Run `cargo fmt` for consistent formatting.
4. Run `cargo clippy` to ensure no warnings/errors.

> [!TIP]
> Only use the following commands (Steps 5 and 6) for larger changes that require comprehensive testing, 
> or if the user explicitly asks. For small edits, rely on `cargo check` and unit tests.

5. Run SonarQube analysis locally to check code quality:
   ```bash
   source .env && npx --yes sonar-scanner -Dsonar.token="$SONARQUBE_TOKEN" -Dsonar.host.url="$SONARQUBE_URL"
   ```
   Results appear at `https://sonarcloud.io/dashboard?id=muxlang_mux-compiler`, please fetch them (this does not require login or auth).
6. Run greptile cli tool for contrast against main via:
   ```bash
   greptile review -b main --plain
   ```

The user will run `cargo test` and insta snapshot tests separately. Do not manually edit the snapshots.

### Common Issues
- Executable output seems cut off → likely a segfault due to incorrect LLVM IR generation. Review codegen changes carefully.

### Hard-Won Facts
- **Coupled runtime testing**: `MUX_RUNTIME_LIB=<path to .a>` overrides the runtime
  library the compiler links. Required whenever compiler work depends on unmerged
  mux-runtime changes. The pre-commit hook runs the full `cargo test`, so export it
  before committing or the executable tests link a stale runtime and fail.
- **Runtime resolution is prebuilt-only**: `MUX_RUNTIME_LIB`, then a library beside
  the compiler binary (or in `../lib`), then the one cargo built into `target/`.
  The compiler never builds a runtime while compiling a program, and always links
  the `full` feature set - static linking discards what a program does not
  reference, so trimming saved nothing and cost a source tree, a build cache, and
  a resolution order deep enough to hide a broken install.
- **Local rc-leak-check: use `scripts/leak-check.sh`.** After any
  reference-counting / codegen change, reproduce the CI "RC Leak
  Check" job locally with `scripts/leak-check.sh [file.mux ...]`. It builds the
  runtime with the `rc-leak-check` feature and FORCES it via `MUX_RUNTIME_LIB`, so a
  leaking program exits 101 ("N reference-counted block(s) still live at exit").
  `rc-leak-check` sits outside mux-runtime's `full` feature on purpose, so the
  library the compiler links by default never carries the assertion. Forcing the
  feature-built archive via `MUX_RUNTIME_LIB` is the only way to run it; without
  that, a leaky program links a plain runtime and falsely exits 0. rc-leak-check
  itself is not broken - the trap is linking the wrong runtime. (Valgrind is the
  other leak gate; `scripts/valgrind-checks.sh`.)
- **Test-script auto-discovery**: every `test_scripts/*.mux` file is picked up by the
  lexer, parser, and executable integration suites (insta snapshots); files under
  `test_scripts/error_cases/` only by the executable suite. Adding a script requires
  `cargo insta test --accept` and a review of the generated snapshots.
- **List indexing is Python-style**: negative indices count from the end on both
  reads and writes. A read is a compile-time error only when it is provably out of
  bounds against a known length (a list literal) - i.e. past the end, or a negative
  that reaches before the start; all other targets normalize the index at runtime
  and panic only when the normalized index is still out of bounds. List writes wrap
  negative indices and extend past the end (`mux_list_set_value` in mux-runtime).
  Static checks (`semantics/const_checks.rs::check_const_index`) and codegen changes
  must keep negatives valid down to `-length`.
- **Compile-time guard rule**: checks built on `semantics/const_fold.rs` must be
  zero-false-positive. The folder returns `None` on anything unprovable (overflow,
  NaN, unknown identifiers) and the runtime check stays the fallback; Mux has no
  warning level, so a wrong compile error blocks valid programs.
- **Sonar gate**: PRs fail CI on any new SonarCloud issue; keep cognitive
  complexity per function at 15 or below.

## Release Process

Versions are independent per repo; there is no root `VERSION` file or sync script.
The compiler version (`mux-compiler/Cargo.toml`, `CARGO_PKG_VERSION`) is the
canonical "Mux version". Preparing a release (changelog, version bump, lockfile)
is agent-safe; tagging, `cargo publish`, and `fly deploy` are MAINTAINER-ONLY -
prepare everything, then hand those to the user.

Full steps:
[muxlang/mux-context release process](https://github.com/muxlang/mux-context/blob/main/docs/release-process.md#mux-compiler).

## System Architecture
The Mux compiler is a workspace with three main partitions:

### mux-compiler (Rust)
Responsible for parsing, semantic analysis, and LLVM IR generation:
- **lexer** - tokenizes source code
- **parser** - builds AST
- **semantics** - type checking and symbol resolution
- **codegen** - generates LLVM IR (output `.ll` files)

Use `mux run -i <file.mux>` to view the generated IR.

### mux-runtime (C/Rust)
Provides runtime support for compiled Mux programs:
- Memory allocation and reference counting
- String operations (UTF‑8)
- Collection implementations (list, map, set)
- Type conversions and utilities
- Standard library

The compiler generates calls to runtime functions; understanding this interface is essential for codegen changes.

## Code Style Guidelines
- Write idiomatic Rust – clean, readable, well‑structured.
- Use Rust's type system to prevent compile‑time errors.
- Prefer `Result<T, E>` for fallible operations.
- Document public APIs with rustdoc comments (`///`).
- Keep functions small and focused.

### Naming Conventions
- **Types**: `PascalCase`
- **Functions/Methods**: `snake_case`
- **Variables**: `snake_case`
- **Constants**: `SCREAMING_SNAKE_CASE`
- **Modules**: `snake_case`
- **Type Parameters**: single uppercase letter (`T`, `U`) or descriptive (`Elem`).
- **Unused variables**: prefix with `_`.

### Error Handling
- Return `Result<T, String>` or `Result<T, Box<dyn Error>>`.
- Use the `?` operator for propagation.
- Provide context: `Err(format!("failed to {}", action))`.
- No `.unwrap()` except in tests.

### Type System
- Use concrete LLVM types (e.g., `i64`, `f64`, `*mut c_char`); do **not** box `*mut Value`.
- Leverage Rust's type system for compile‑time safety.
- Avoid unnecessary boxing or dynamic dispatch.
- Use `Option<T>` for nullable values.

### Comments
- `///` for public API documentation.
- `//` for implementation notes.
- Document *why*, not *what*.
- Do not state the obvious.

## Project Structure
Key directories:
- `mux-compiler/src/` – compiler implementation.
- `test_scripts/` – sample Mux programs.

The runtime library and the documentation live in separate repositories
(`muxlang/mux-runtime`, `muxlang/mux-website`), not in this tree. The runtime is
a git dependency pinned by `Cargo.lock`; see the mux-runtime section of
[CONTRIBUTING.md](CONTRIBUTING.md) for how it resolves and how to move the pin.

## Key Constraints
- No dynamic typing.
- No implicit type conversions.
- No runtime reflection.
- All generics monomorphize at compile time.
- Interfaces use static dispatch (no vtables).

## Codegen Module Architecture
- Submodules import from `crate::ast`, not `crate::parser`.
- Visibility: `pub(super)` for submodule functions, `fn` for private helpers.
- Memory management uses reference counting (see `memory.rs`).
- Expressions vs. statements: expressions return values; statements perform actions.
- Boxing/unboxing: all primitive values are boxed into `*mut Value` pointers.
- Three type representations: AST (`TypeNode`), semantic (`Type`), LLVM (`BasicTypeEnum`).

## When to Ask for Clarification
- Unclear requirements or specifications.
- Language design questions (check README.md first).
- Architectural decisions affecting multiple components.
- Trade‑offs between correctness and performance.
- Changes to existing public APIs.
- Anything that seems like a "hack" or workaround.

## Workflow for New Work
1. Check README.md if confused about language design.
2. Read existing relevant code to understand patterns.
3. Implement the feature using best Rust practices.
4. Run `cargo build` to verify compilation.
5. Run `cargo run -- test_scripts/test_file.mux` to test functionality.
6. Run `cargo clippy` to ensure no warnings/errors.
7. Let the user run `cargo test` for comprehensive testing.
8. After tests pass, update documentation (website and root README as needed).

**Add to this document as you learn vital information.**
