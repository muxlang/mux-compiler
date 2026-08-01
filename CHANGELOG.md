# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **`list` and `set` satisfy `Collection<T>`**: the generic `std.dsa.algorithm`
  functions (`sort`, `binary_search`, `reverse`, `index_of`, `unique`, `max`,
  `min`, `any`, `all`, `count`, `sum_ints`, `sum_floats`) now accept a plain
  `list` or `set`, not only a `Collection` class such as `stack.Stack`. Builtin
  collections previously satisfied no interface at all, so passing one failed
  with a confusing "Undefined variable 'items'". `list` gains `len()`,
  `contains()`, and `to_list()`; `set` and `map` gain `len()`. Closes #277.

### Changed
- **BREAKING: `clear()` removed from the `Collection<T>` interface**. It was the
  one member no builtin collection could provide (the runtime has no
  `mux_*_clear`) and it was unused by every stdlib algorithm, so requiring it
  kept `list` and `set` out of the interface. `Collection` is now a read-oriented
  view (`len`, `is_empty`, `to_list`, `contains`); implementations may still
  offer their own `clear()` as a regular method, and the dsa classes do.

### Fixed
- **Windows links against the import library, not a stray DLL**: runtime
  resolution accepted any directory containing `mux_runtime.dll` as the one to
  link against. A packaged install must keep that DLL beside `mux.exe` for the
  loader to find it, so `bin/` always won over the `lib/` holding
  `mux_runtime.lib` - and a bare DLL is not a linker input on Windows, so every
  compile failed with `LNK1181: cannot open input file 'mux_runtime.lib'`. A
  dynamic library now only counts as linkable where it actually is, which is
  everywhere except Windows.
- **Compiling a program on Windows now links**: the temporary object file was
  created with `FILE_FLAG_DELETE_ON_CLOSE` and its handle held open across the
  link. While such a handle is open, Windows fails any open that does not itself
  pass `FILE_SHARE_DELETE` - and `link.exe` does not - so every compile ended in
  `LNK1104: cannot open file` naming an object that existed and was readable.
  The share mode on the creating handle cannot grant that; only the opener's can.
  Windows now keeps the object named, closes the handle before linking, and
  removes it afterwards, including on the codegen-failure path. The unix path is
  unchanged: it still unlinks at creation and passes the descriptor as the
  linker's stdin, which has no Windows equivalent.
- **Windows no longer receives ELF-only linker options**: `-Wl,-rpath`,
  `-Wl,--disable-new-dtags` and `-Wl,--gc-sections` were passed on every
  platform. MSVC's linker answered each with `LNK4044: unrecognized option` and
  ignored it. Windows resolves a DLL from the executable's own directory, and
  MSVC dead-strips by default, so the flags are simply not emitted there.
- **A failed link now reports the linker's own error, not just its exit code**:
  the internal-error report was built from clang's stderr alone. `ld` and `lld`
  write diagnostics there, but MSVC's `link.exe` writes them to stdout, so on
  Windows the entire report was `linking failed: clang-22: error: linker command
  failed with exit code 1104` - the `LNK1104: cannot open file '...'` line naming
  the actual problem was discarded. Both streams are reported now. Found when the
  packaged-artifact smoke test was first run on Windows, where a link failure was
  undiagnosable from CI logs.
- **Same-named globals in different modules no longer collide**: module-level
  globals were emitted and keyed by their bare name, so two imported modules
  each declaring e.g. `const int SHARED` shared one slot and both resolved to
  whichever was declared last - silently returning the wrong value from a
  module's own functions as well as through its namespace. Each module's globals
  are now emitted as `module!name` into a per-module table, and only the owning
  module's globals are visible while its init and functions are generated.
  Namespaced reads resolve through the symbol's mangled name, so an aliased
  import (`import shapes.circle as c`) resolves correctly too. Closes #279.
- **`map.to_list()`**: reported "Undefined method" despite being listed as added
  in 0.4.0 (#209); `set.to_list()` worked. It now returns the map's key/value
  pairs, matching `get_pairs()`.
- **Type parameters inferable through a builtin's bound**: a signature whose type
  parameter appears only in a bound (e.g.
  `reverse<T, E is Collection<T>>(E c) returns list<T>`) could not infer `T` when
  `E` was a builtin, because only class types exposed their type arguments to
  bound-driven inference. Removes a dead inference placeholder that was hardcoded
  to `None`.

## [0.6.0] - 2026-07-18

### Changed
- **BREAKING: `{}` is now always the empty set; the empty map is `{:}`**. `{}` in
  a map-typed position no longer resolves to an empty map - it is a compile error
  that points at `{:}`, and the reverse (`{:}` where a set is expected) reports
  the same way. Migration is mechanical: `map<K,V> m = {}` becomes
  `map<K,V> m = {:}`. Set literals are unaffected. This supersedes the contextual
  `{}` resolution added in 0.5.0: `Type::EmptySetOrMap` and its span-keyed
  override map are removed, so the set/map ambiguity no longer reaches semantics
  or codegen. Closes #266.
- **Valgrind and benchmark PR reports render as charts**: the PR comment now shows
  a leak split pie, per-phase median bars, and an exact-numbers table instead of
  raw log dumps; raw logs stay in the collapsed details. All rendered values are
  whitelisted or numeric-validated since Build artifacts are fork-controlled (#267).
- **CI fails on orphaned insta snapshots**: deleting or renaming a `test_scripts/`
  file no longer strands its snapshot as phantom coverage; 52 accumulated orphans
  were deleted and the gate keeps new ones out (#271).

### Added
- **`if` expressions support `else if` chains and multi-line branches**: an `if`
  used in expression position (`auto g = if c { a } else { b }`) previously
  accepted only a single-line, single-`else` form. It now accepts `else if`
  chains and branches whose value spans multiple lines, matching the statement
  form. Each branch is still a single value expression; a chain parses as a
  nested if-expression, so no new AST shape, semantics, or codegen was needed
  (branch type-agreement checks still apply). Closes #281.

### Fixed
- **`++`/`--` on a captured variable inside a closure**: a standalone `count++`
  in a lambda body was rejected by the parser's no-postfix-in-expression guard as
  if it were nested in an expression, even though a bare `x++` statement is valid
  anywhere. The guard now only rejects a postfix `++`/`--` genuinely nested inside
  a larger expression (e.g. `a + y++`), which stays disallowed by design. Codegen
  already handled the captured increment. Closes #280.
- **Match-arm bindings no longer leak past the match**: a match-arm pattern
  binding (e.g. `n` in `n if n % 2 == 0`) or an arm-body local stayed in the
  codegen variable table after the match, so a later declaration reusing the
  name (`auto n = 3`) reused the arm-local slot. That slot's alloca lived in a
  conditional arm block that did not dominate the later store, so the program
  failed with invalid LLVM IR ("instruction does not dominate all uses"). Match
  bindings are now scoped to the match; a subsequent same-named declaration gets
  a fresh slot. Reassignment of an outer variable inside an arm still persists.
- **Recursive and mutually-recursive functions in imported modules**: a call to
  a function in the same imported module (including a recursive self-call) was
  emitted against the bare, unmangled LLVM name and failed with "Undefined
  function: <name>". The current-function name recorded while generating an
  imported module body is now the mangled `module!name`, so same-module calls
  resolve through the existing nested-name logic. A related failure - a
  non-identifier argument such as `n - 1` in a recursive call reporting
  "Undefined variable" - is fixed by resolving argument types through the
  codegen fallback tables when the analyzer's function scope is unavailable.
- **Cross-module constant access**: reading a `const` defined in an imported
  module through the module namespace (`math.PI`) now compiles. Previously only
  stdlib module constants resolved; a user module constant fell through to
  "Field access not supported for expression type Identifier". The module's own
  code could already read the constant; only the namespaced access from an
  importing file was missing. A same-named local in the caller does not shadow
  the module constant. (Known limitation: two imported modules that declare the
  same-named constant collide under the flat global-name model, tracked in #279.)
- **Compound assignment and increment on class fields**: `self.value++`,
  `self.value += n`, and the `obj.field` forms now compile. Previously `++`/`--`
  on a field failed codegen with "Cannot increment on non-identifier" and `+=`/`-=`
  with "Assignment to non-identifier/deref not implemented", even though semantic
  analysis accepted them. Both now route through the same field-store path as a
  plain `obj.field = ...` assignment, so reference counting and where-clause field
  invariants still apply.
- **`auto` now rejects nested empty collections consistently**: `auto x = {1: []}`
  and `auto x = [[]]` previously passed semantic analysis with an unresolved
  element type, while the set/map equivalents were caught. All empty collection
  kinds are now guarded.
- **Empty-collection inference help text**: suggested `{}` for every collection,
  including lists (`list<int> myVar = {}`). Each kind now shows its own literal
  syntax (`[]`, `{:}`, `{}`).

### Security
- **Lockfile bump for the postgres stack**: `postgres-protocol` 0.6.11 -> 0.6.12
  (RUSTSEC-2026-0179 SCRAM CPU-exhaustion DoS, RUSTSEC-2026-0180 hstore decode
  panic) and `tokio-postgres` 0.7.17 -> 0.7.18 (RUSTSEC-2026-0178 short DataRow
  panic); `postgres-types` 0.2.13 -> 0.2.14 moved with the stack (no advisory of
  its own). Transitive via mux-runtime's `postgres` dependency; `cargo audit` is
  clean again.

## [0.5.0] - 2026-07-13

This release completes the split of the former monorepo into independent repos.
`mux-compiler` is now the canonical "Mux version" and resolves/reports the runtime
version independently; the runtime, website, playground API, and syntax
highlighting each live in and version from their own repos. Requires
`mux-runtime` 0.5.0.

### Added
- **Where-clause constraints**: `where { expr, ... }` attaches runtime constraints to
  functions, methods, lambdas, interface methods, enum variants, fields, and classes.
  Provable violations are compile errors (zero-false-positive); the runtime panic is the
  fallback. Closes #224 (#235).
- **Reject provable runtime panics at compile time**: The compiler now turns provable
  runtime panics into compile-time errors, using the zero-false-positive const-fold path.
  Closes #238.
- **`cargo bench` harness (criterion)**: Compiler-phase benchmarks (`lex`, `parse`, `semantics`,
  `codegen`, end-to-end `pipeline`) run over the whole compiling `test_scripts` corpus, plus
  end-to-end `execution` throughput benchmarks for a curated set of compiled workloads. A
  stdlib-only `scripts/bench-report.py` aggregates the per-file medians into a box-and-whisker
  per phase. Benchmarks are local/manual and a non-blocking CI report, never a merge gate.
  Closes #247.
- **Valgrind memory checking**: CI now runs compiled programs and the compiler under Valgrind
  (Memcheck), gating on definite/indirect leaks and memory errors with checked-in suppressions
  for benign third-party noise. Closes #246.
- **Range-literal syntax diagnostic**: A targeted diagnostic for a digit followed by `..`
  (range-literal syntax) replaces the previous generic parse error. Closes #245.
- **Independent runtime version resolution**: The compiler resolves and reports the runtime
  version independently of its own version, so `mux --version` reports both (e.g.
  `mux 0.5.0 (runtime 0.5.0)`).

### Changed
- **Repo split from the monorepo**: Extracted the runtime, website, playground API, and syntax
  highlighting into their own repos; removed the root `VERSION` file and `sync-version` tooling;
  scoped the SonarCloud analysis to the compiler; and rebound the project identity to
  `muxlang/mux-compiler`. Internals documentation now points at `muxlang/mux-context`.
- **CLI polish**: Colored diagnostics, styled help output, doctor glyphs, a version banner, and
  a compile spinner. Closes #249.
- **Runtime panic documentation and behavior updated** (#225, #230).
- **Removed ad-hoc perf tooling**: Deleted `scripts/measure-baseline.sh`,
  `scripts/check-timings.py`, and the orphaned `infra/ci/baselines/*.json` timing budgets, and
  stripped the unused `--timings-file` plumbing from `run-checks.sh` / `integration-checks.sh`.
  Also removed the unused `cargo-fuzz` setup under `mux-compiler/fuzz/`.
- **Slimmed the README** to a real README and pointed internals at `muxlang/mux-context`.

### Fixed
- **`mux build` / `mux run` now fail on link errors**: A failed `clang` link previously printed
  the error but exited 0 (`build` reported success with no executable; `run` could fall through
  and execute a stale binary from a prior build). `report_clang_output_or_exit` now exits
  non-zero as its name implies.
- **Reference-counting correctness**: Reference-count cleanup for owned temporaries plus
  init/copy correctness fixes (#253); closures are now allocated with a reference-count header.
  Closes #250 (#252).
- **Collection read-path performance and correctness**: Fixed O(n^2) indexed reads,
  runtime-cache staleness, and Python-style negative indexing on collection reads
  (#256, #258, #259, #260).
- **`match` usable as a statement**: `match` now works as a statement and the guarded-arm
  exhaustiveness hole is closed (#233, #234, #243) (#242).
- **Trailing commas**: Allow trailing commas before newline-closed collection delimiters (#244).
- **Enum codegen**: Load enums from boxed pointers correctly in codegen (#237).
- **Release checksum verification**: Verify release checksums by hash, not path.

### Security
- **`cmov` 0.5.3 -> 0.5.4**: Bumped to fix GHSA-3rjw-m598-pq24 (#254).

## [0.4.1] - 2026-06-27

### Fixed
- **Windows CI linker failure (`xml2.lib`)**: The conda-forge `libxml2` packages do not install any `.lib` import library into `Library/lib/`, causing `LNK1181: cannot open input file 'xml2.lib'` on `windows-latest` runners. Fixed by adding a dedicated step after MSVC toolchain setup that generates `xml2.lib` from the installed `libxml2*.dll` using `dumpbin /exports` and `lib.exe`.

## [0.4.0] - 2026-06-26

### Added
- **Mux AI documentation assistant**: In-docs chat widget powered by a Cloudflare Worker (RAG over `mux-website/docs/` via Vectorize + Llama 3.3 70B). Answers Mux questions with citations, explains compiler errors, and rejects off-topic queries. Includes `tools/docs-indexer/` for re-indexing and `tools/retrieval-test/` eval harness (8/8 retrieval, 19/19 error-explainer). Full runbook in `workers/mux-ai/README.md`.
- **DSA stdlib expanded**: Added `algorithm.mux` (generic graph algorithms: topological sort, cycle detection, DFS, BFS), `graph.mux` (adjacency-list directed graph), `bintree.mux` (binary tree with inorder/preorder/postorder traversals), `heap.mux` (min/max heap), `queue.mux` (FIFO), `stack.mux` (LIFO), and `collection.mux` (base Collection interface). Closes #203.
- **`to_char()` conversion method**: Implemented `string.to_char() -> result<char, string>` and `int.to_char() -> char` (Unicode code-point to char). Closes #207.
- **`to_list()` on set and map**: `set<T>.to_list()` and `map<K,V>.to_list()` now registered and callable. Closes #209.

### Changed
- **LLVM upgraded from 17 to 22**: Migrated inkwell dependency and all CI/build tooling to LLVM 22, which is more broadly available and actively maintained. Closes #215.
- **Dead code elimination**: Unused symbols (variables, classes, enums, functions, generics) are no longer emitted to LLVM IR, reducing binary size and intermediate output. Closes #200.
- **Minimal end-user installation**: End-user installs now ship only the compiler binary and runtime; development tooling (LLVM, clang, analysis tools) is separated into the dev setup path. Closes #193.
- **God Object refactor**: Broke down oversized structs/impls in the compiler (semantic analyzer, codegen context) into smaller focused components. Closes #194.
- **Improved error messages for collection types**: Set and map type errors now display `set<T>` and `map<K,V>` instead of raw brace-syntax (`{char}`, `{string: int}`). Closes #210.
- **Improved error for `.new()` on built-in collections**: Calling `list.new()`, `map.new()`, or `set.new()` now emits a helpful diagnostic suggesting `[]` or `{}` literal syntax instead of a generic undefined-type error. Closes #204.
- **SonarQube and Greptile cleanups**: Addressed code quality findings across multiple passes: god-object decomposition, vulnerability dependency updates, and ESLint/security-hotspot fixes in the website.

### Fixed
- **`void` functions require explicit `return`**: Functions declared `returns void` without a `return` statement now produce a compile-time error instead of silently compiling. Closes #211.
- **Map `{}` literal compiled as Set**: `map<K,V> m = {}` previously produced a `Value::Set` at runtime, causing segfaults on map operations. Fixed by resolving `{}` type contextually during semantic analysis (`SetOrMapLiteral`) so codegen emits `mux_new_map` vs `mux_new_set` correctly.
- **Struct layout corruption in interface-implementing classes**: Inline constructor initialization used positional field indices instead of the interface-aware field map, causing the first real field's data to overwrite the vtable slot. Affected all classes implementing interfaces.
- **Non-primitive field initialization in class constructors**: Non-generic class constructors (e.g., `Graph.new()`) zero-initialized `list`/`map`/`set` fields as null instead of real empty collections.
- **Generic class vtable generation crash**: `generate_class_vtables()` attempted to build vtables using unspecialized method names (e.g., `Graph.len`) which do not exist; generic classes only have monomorphized instances. Vtable generation is now skipped for generic classes (interfaces use static dispatch).
- **Cross-module import ordering**: `collect_hoistable_declarations()` ran before imports were resolved, so classes in a file could not see imported interfaces during the hoisting pass. Imports are now processed during hoisting; `expression_type_overrides` from submodules are also merged so empty `{}` literals are correctly disambiguated. Closes #203.
- **`Type::Module` panic**: `resolve_type_with_seen()` and `llvm_type_from_resolved_type()` panicked on `Type::Module` instead of returning `Err`, breaking `module.CONST.method()` call patterns.
- **Website frontend examples**: Audited and corrected all code examples and interactive demos on the documentation site; removed a stale debug log from the compiler.
- **Dependency vulnerabilities**: Updated website and tooling dependencies to resolve known CVEs.

## [0.3.2] - 2026-06-13

### Changed
- **SonarQube quality issues resolved**: Replaced `unreachable!()` in `deep_clone_value` for `Value::Object` inside containers, fixed UB in sync unlock arm, replaced 7 `.expect()` calls with proper error propagation, and extracted duplicate constructor helpers.
- **Code duplication reduced**: Overall project duplication dropped from 4.5% to 3.9%. Extracted module-level expression helpers in `methods.rs`, merged duplicate equality and return value arms in `statements.rs`, and added signature macros to compact ~40 runtime function declarations.
- **Version metadata updated**: All configuration files bumped from 0.3.1 to 0.3.2.

### Fixed
- **Segfault when running `cargo test`**: `LD_LIBRARY_PATH` was checked before `DT_RUNPATH`, so the workspace `.so` was loaded instead of the cached release `.so`. Added `-Wl,--disable-new-dtags` to force `DT_RPATH`, which is checked before `LD_LIBRARY_PATH`.
- **LLD linker flags**: Removed `-no-pie` flag to fix LLD compatibility on modern Linux distributions.

## [0.3.0] - 2026-05-07

### Added
- **Syntax highlighting support**: Added TextMate and Tree-sitter grammar support with setup guidance for VSCode, Sublime Text, JetBrains, Neovim, and Helix.
- **Setup documentation**: New `mux-website/docs/setup.md` with language installation and editor configuration guides.

### Changed
- **Profiling decoupled**: Removed built-in profiling infrastructure (`mux-profiling` crate) from compiler and runtime. Profiling now uses external tools (perf, Instruments, WPA) only.
- **Code quality improvements**: Pinned GitHub Actions versions, added `--locked` to cargo commands, added Cargo.lock files, refactored Python and JavaScript generators to fix SonarQube findings.

### Fixed
- **Code review cleanup**: Removed orphaned profiling scripts, cleaned up empty scope blocks in compiler, and fixed numbered list in CONTRIBUTING.md.

## [0.2.1] - 2026-04-22

### Changed
- **Compiler maintainability work**: Reduced complexity across compiler modules with a broad cleanup and refactor pass.
- **Standard library internals**: Refactored and optimized stdlib implementations for better consistency and maintainability.
- **Developer workflow and project metadata**: Updated AI agent guidance, OpenCode configuration, and supporting repository automation files.
- **Documentation and website updates**: Improved README content and landing page structure, examples, and installation guidance.

### Fixed
- **Codegen regressions**: Fixed recent LLVM IR generation regressions and related import handling issues.
- **Website behavior**: Corrected landing page rendering details, including list key usage and stack example behavior.
- **Build and CI support scripts**: Fixed tooling and script issues affecting local and CI workflows.
- **Versioning release prep**: Synced release metadata and version-related files for `0.2.1`.

### Security
- **Dependency and vulnerability updates**: Applied dependency maintenance and vulnerability fixes, including Dependabot-driven updates.
- **Static analysis cleanup**: Addressed SonarCloud findings and code quality issues across the codebase.

## [0.2.0] - 2026-03-24

### Added
- **Standard library**: Full implementation of standard library modules (`math`, `io`, `net`, `sql`, `random`, `datetime`, `dsa`).
- **Data structures library**: New `dsa` module with binary tree, graph, and other data structures.
- **SQL support**: SQL client functionality for database interactions.
- **HTTP client**: Built‑in HTTP client for making web requests.
- **Network server architecture**: Foundation for building network servers.
- **JSON, CSV, and environment utilities**: Tools for handling JSON, CSV, and environment variables.
- **Networking primitives**: Low‑level networking building blocks.
- **IO stdlib library**: Standard I/O operations.
- **Error message improvements**: More helpful and context‑aware error messages.
- **Refactored codebase to Rust idioms**: Improved readability and maintainability.
- **CI improvements**: Fixed continuous integration pipelines.
- **Project tooling & hooks**: Updated pre‑commit hooks and development tooling.

### Changed
- **Upgraded to LLVM 17** (already present, but now formally documented).
- **Improved installation process**: Better installer scripts and platform detection.
- **Simplified project structure**: Cleanup of repository layout.

### Fixed
- **Numerous bug fixes** across the compiler and runtime.
- **Reference counting issues**: Fixed memory management bugs.
- **Type checking edge cases**: Corrected handling of complex type scenarios.
- **Code generation correctness**: Fixed issues with LLVM IR generation.
- **Exhaustiveness checking in match statements**: Guards and wildcards now work correctly.
- **Class and interface resolution**: Fixed bugs in type hierarchy.

### Security
- **Resolved dependabot alerts** (see PR #140).

## [0.1.2] - 2026-02-08

### Added
- **Match as switch statement**: Extended `match` to work as a switch statement for any type (not just enums).
- **Improved pattern matching**: Enhanced exhaustiveness checking and guard support.

### Fixed
- **Reference and chaining fixes**: Resolved issues with reference handling and method chaining.
- **Function return handling**: Corrected return value processing.
- **Class‑related bugs**: Fixed errors in class instantiation and inheritance.
- **Frontend cleanup**: Removed erroneous information from error messages.

## [0.1.1] - 2026-02-07

### Fixed
- **Crates.io publishing**: Fixed configuration and metadata for publishing to crates.io.
- **Build updates**: Adjusted build scripts for proper release artifacts.

## [0.1.0] - 2026-02-07

### Added
- **Initial public release** of the Mux compiler and runtime.
- **Core language features**: Static typing, generics, pattern matching, error handling (`result<T,E>`, `optional<T>`).
- **LLVM‑based code generation**: Produces native executables.
- **Reference‑counted memory management**: Automatic memory safety.
- **Basic standard library**: Collections, string operations, I/O.
- **Installer scripts** for Linux, macOS, and Windows.
- **Documentation website** (mux‑lang.dev) with language specification.

### Known Issues
- No LSP or code formatter yet.
- Standard library is minimal.
- Breaking changes expected.
