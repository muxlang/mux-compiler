use std::collections::HashMap;
use std::sync::OnceLock;

/// Kind of standard library module implementation
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdModuleKind {
    /// Module symbols come from stdlib.rs (native Rust implementations)
    RuntimeBacked,
    /// Module comes from embedded .mux source files
    Embedded,
}

/// Definition of a standard library module
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StdModuleDef {
    /// Full module name (e.g., "std.math", "std.dsa.stack")
    pub name: &'static str,
    /// How the module is implemented
    pub kind: StdModuleKind,
    /// Runtime features required to use this module (empty if none)
    pub runtime_features: &'static [&'static str],
}

const RUNTIME_STD_MODULES: &[(&str, &[&str])] = &[
    ("std.assert", &[]),
    ("std.datetime", &[]),
    ("std.io", &[]),
    ("std.math", &[]),
    ("std.random", &[]),
    ("std.env", &[]),
    ("std.net", &["net"]),
    ("std.net.http", &["net"]),
    ("std.sync", &["sync"]),
    ("std.sql", &["sql"]),
    ("std.data", &["json", "csv"]),
    ("std.data.json", &["json"]),
    ("std.data.csv", &["csv"]),
];

const EMBEDDED_STD_MODULES: &[&str] = &[
    "std.dsa",
    "std.dsa.algorithm",
    "std.dsa.bintree",
    "std.dsa.collection",
    "std.dsa.graph",
    "std.dsa.heap",
    "std.dsa.queue",
    "std.dsa.stack",
];

fn insert_std_module(
    registry: &mut HashMap<&'static str, StdModuleDef>,
    name: &'static str,
    kind: StdModuleKind,
    runtime_features: &'static [&'static str],
) {
    registry.insert(
        name,
        StdModuleDef {
            name,
            kind,
            runtime_features,
        },
    );
}

/// Registry of all standard library modules.
/// This is the single source of truth for what std modules exist and their properties.
fn build_std_module_registry() -> HashMap<&'static str, StdModuleDef> {
    let mut registry = HashMap::new();

    for (name, runtime_features) in RUNTIME_STD_MODULES {
        insert_std_module(
            &mut registry,
            name,
            StdModuleKind::RuntimeBacked,
            runtime_features,
        );
    }

    for name in EMBEDDED_STD_MODULES {
        insert_std_module(&mut registry, name, StdModuleKind::Embedded, &[]);
    }

    registry
}

pub fn std_module_registry() -> &'static HashMap<&'static str, StdModuleDef> {
    static REGISTRY: OnceLock<HashMap<&'static str, StdModuleDef>> = OnceLock::new();
    REGISTRY.get_or_init(build_std_module_registry)
}
