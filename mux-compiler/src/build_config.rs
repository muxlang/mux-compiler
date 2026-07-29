/// Absolute path to the runtime archive `build.rs` found (or expected) in
/// cargo's target directory. Only the containing directory is used; the
/// platform library names live in `main.rs`.
pub static MUX_RUNTIME_STATIC: &str = env!("MUX_RUNTIME_STATIC");
