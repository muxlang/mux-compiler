<div align="center">

<img src="https://mux-lang.dev/img/mux-logo.png" alt="Mux Logo" width="120">

# Mux

**The Programming Language For Everyone**

[![Version](https://img.shields.io/badge/version-0.6.0-blue.svg?style=flat-square)](https://github.com/muxlang/mux-compiler/releases)
[![License](https://img.shields.io/badge/license-MIT-green.svg?style=flat-square)](LICENSE)
[![crates.io](https://img.shields.io/crates/v/mux-lang.svg?style=flat-square)](https://crates.io/crates/mux-lang)
[![Documentation](https://img.shields.io/badge/docs-online-blue.svg?style=flat-square)](https://mux-lang.dev)
[![Status](https://img.shields.io/badge/status-alpha-orange.svg?style=flat-square)]()
[![Sonar Quality Gate](https://sonarcloud.io/api/project_badges/measure?project=muxlang_mux-compiler&metric=alert_status)](https://sonarcloud.io/summary/new_code?id=muxlang_mux-compiler)
[![Coverage](https://sonarcloud.io/api/project_badges/measure?project=muxlang_mux-compiler&metric=coverage)](https://sonarcloud.io/summary/new_code?id=muxlang_mux-compiler)

</div>

**Mux** is a statically-typed, reference-counted programming language that combines Python's readability, Go's simplicity, and Rust's type safety into one cohesive package. Write fast, safe, and maintainable code without the complexity.

---

## Why Mux?

- **Simple & Readable:** Clean syntax without semicolons, Python-like readability with Go-inspired minimalism
- **Type Safe:** Strong static typing with no implicit conversions. Catch errors at compile time
- **Fast & Native:** LLVM-powered compilation delivers native performance
- **Memory Safe:** Reference-counted memory management provides safety without GC pauses or complex ownership
- **Modern Features:** Full-featured with generics, interfaces, tagged unions, and pattern matching
- **Developer Friendly:** Helpful error messages, built-in tooling, and comprehensive documentation

---

## Quick Start

### Installation (One-Line)

```bash
curl -fsSL https://raw.githubusercontent.com/muxlang/mux-compiler/main/scripts/install.sh | sh
```

[Full installation guide](https://mux-lang.dev/docs/getting-started/quick-start) | [Other options](#installation)

### Your First Program

Create `hello.mux`:

```mux
func main() returns void {
    print("Hello, Mux!")
}
```

Run it:

```bash
mux run hello.mux
```

---

## Documentation

The full language reference, guides, and standard-library docs live at
**[mux-lang.dev](https://mux-lang.dev)**:

- **[Getting Started](https://mux-lang.dev/docs/getting-started/quick-start)** - Installation and first steps
- **[Setup](https://mux-lang.dev/docs/setup)** - Full install, editor setup, and tooling
- **[Language Guide](https://mux-lang.dev/docs/language-guide/overview)** - Complete language overview
- **[Reference](https://mux-lang.dev/docs/reference/overview)** - Lexical structure, expressions, statements, memory model
- **[Standard Library](https://mux-lang.dev/docs/stdlib/)** - Built-in modules and functions

For how the compiler and runtime work internally (design rationale, the value
model, codegen, memory), see the **[muxlang/mux-context](https://github.com/muxlang/mux-context)**
knowledge hub.

---

## Installation

Choose the method that works best for you:

### Option 1: Prebuilt Binaries (Recommended)

The fastest way to get started. No Rust or LLVM required.

**Linux & macOS:**
```bash
curl -fsSL https://raw.githubusercontent.com/muxlang/mux-compiler/main/scripts/install.sh | sh
```

**Windows (PowerShell):**
```powershell
iwr -useb https://raw.githubusercontent.com/muxlang/mux-compiler/main/scripts/install.ps1 | iex
```

**Custom installation directory:**
```bash
MUX_INSTALL_DIR=/usr/local/bin MUX_LIB_DIR=/usr/local/lib sh install.sh
```

### Option 2: From crates.io

For Rust developers who already have cargo installed:

```bash
cargo install mux-lang
```

Note: LLVM 22 and clang must be installed first for source builds.

### Option 3: Build from Source

For contributors and those who want the latest features:

```bash
git clone https://github.com/muxlang/mux-compiler
cd mux-compiler
./scripts/bootstrap-dev.sh   # Installs LLVM 22 automatically
./scripts/dev-cargo.sh build
```

### Verify Installation

```bash
mux --version          # Check version
mux doctor             # Validate runtime dependencies
mux doctor --dev       # Validate LLVM 22 and clang for development
```

---

## Quick Example

Mux combines familiar syntax with powerful features:

```mux
// Error handling with Result types
func divide(int a, int b) returns result<int, string> {
    if b == 0 {
        return err("division by zero")
    }
    return ok(a / b)
}

// Pattern matching with exhaustive checking
func main() returns void {
    auto result = divide(10, 2)

    match result {
        ok(value) {
            print("Result: " + value.to_string())
        }
        err(error) {
            print("Error: " + error)
        }
    }
}
```

**More examples:** see [`test_scripts/`](./test_scripts/) or the [Language Guide](https://mux-lang.dev/docs/language-guide/overview).

---

## How it works

```
.mux --> lexer --> parser --> semantics --> LLVM codegen --> .ll --> clang --> native binary
                                                                   (links libmux_runtime)
```

The compiler emits LLVM IR and invokes `clang` to produce a native binary that
links the [runtime](https://github.com/muxlang/mux-runtime) (reference counting,
strings, collections, stdlib). Use `mux run -i <file.mux>` to view the generated
IR. The deeper design notes (value representation, monomorphization, the object
system, memory model) live in [muxlang/mux-context](https://github.com/muxlang/mux-context/tree/main/docs/design).

---

## Contributing

Mux is an open-source project and welcomes contributions! Whether you're adding features, fixing bugs, improving documentation, or testing, your help is valuable.

- Read [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines
- Check [GitHub Issues](https://github.com/muxlang/mux-compiler/issues) for tasks
- Join [GitHub Discussions](https://github.com/muxlang/mux-compiler/discussions) for questions and ideas

### Profiling

Profiling is done with external tools so it stays decoupled from the compiler and runtime. See the Setup page for platform-specific guidance.

---

## Project Status

⚠️ **Alpha Stage**: Mux is actively being developed. Expect breaking changes and incomplete features as we work toward a stable release.

- **Current Version:** 0.6.0

## Versioning

The compiler version lives in `mux-compiler/Cargo.toml` (`CARGO_PKG_VERSION`) and is
the canonical "Mux version" - there is no separate version file.

- When releasing, bump `version` in `mux-compiler/Cargo.toml`, update the badge and
  the `- **Current Version:**` line above, then run `cargo build` to refresh `Cargo.lock`.
- The runtime versions independently (see [muxlang/mux-runtime](https://github.com/muxlang/mux-runtime)); `mux --version` reports both compiler and runtime.
- Full release steps: [muxlang/mux-context](https://github.com/muxlang/mux-context/blob/main/docs/release-process.md#mux-compiler).
- **License:** MIT
- **Maintainer:** [Derek Corniello](https://github.com/DerekCorniello)

---

## Runtime ABI Note

The runtime uses a unified representation for `optional<T>` and `result<T, E>` at the FFI level (boxed `Value` pointers). Compiler-generated code and FFI should treat optionals/results as `*mut Value` and use the runtime discriminant helpers when inspecting variants. See [error-handling](https://github.com/muxlang/mux-context/blob/main/docs/design/error-handling.md).

---

## Related repositories

| Repo | What it is |
|------|------------|
| [mux-runtime](https://github.com/muxlang/mux-runtime) | Runtime + standard library linked by compiled programs |
| [mux-website](https://github.com/muxlang/mux-website) | Docs site (mux-lang.dev) and the language reference |
| [mux-website-api](https://github.com/muxlang/mux-website-api) | Compile/run API behind the playground |
| [tree-sitter-mux](https://github.com/muxlang/tree-sitter-mux) | Tree-sitter grammar + highlight queries |
| [mux-syntax-highlighting](https://github.com/muxlang/mux-syntax-highlighting) | TextMate grammar, VSCode extension, canonical syntax spec |
| [mux-context](https://github.com/muxlang/mux-context) | Cross-repo architecture, design rationale, glossary, releases |

## License

Mux is licensed under the [MIT License](LICENSE).
