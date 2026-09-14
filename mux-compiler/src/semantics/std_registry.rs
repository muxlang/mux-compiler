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
}

const RUNTIME_STD_MODULES: &[&str] = &[
    "std.datetime",
    "std.io",
    "std.fs",
    "std.math",
    "std.random",
    "std.env",
    "std.net",
    "std.net.url",
    "std.net.tls",
    "std.net.http",
    "std.net.websocket",
    "std.sync",
    "std.process",
    "std.log",
    "std.regex",
    "std.uuid",
    "std.crypto",
    "std.cli",
    "std.sql",
    "std.data",
    "std.data.json",
    "std.data.csv",
];

const EMBEDDED_STD_MODULES: &[&str] = &[
    "std.dsa",
    "std.dsa.algorithm",
    "std.dsa.bintree",
    "std.dsa.collection",
    "std.dsa.deque",
    "std.encoding",
    "std.dsa.graph",
    "std.dsa.heap",
    "std.dsa.priority_queue",
    "std.dsa.queue",
    "std.dsa.stack",
    "std.dsa.trie",
    "std.dsa.union_find",
    "std.dsa.weighted_graph",
];

fn insert_std_module(
    registry: &mut HashMap<&'static str, StdModuleDef>,
    name: &'static str,
    kind: StdModuleKind,
) {
    registry.insert(name, StdModuleDef { name, kind });
}

/// Registry of all standard library modules.
/// This is the single source of truth for what std modules exist and their properties.
fn build_std_module_registry() -> HashMap<&'static str, StdModuleDef> {
    let mut registry = HashMap::new();

    for name in RUNTIME_STD_MODULES {
        insert_std_module(&mut registry, name, StdModuleKind::RuntimeBacked);
    }

    for name in EMBEDDED_STD_MODULES {
        insert_std_module(&mut registry, name, StdModuleKind::Embedded);
    }

    registry
}

pub fn std_module_registry() -> &'static HashMap<&'static str, StdModuleDef> {
    static REGISTRY: OnceLock<HashMap<&str, StdModuleDef>> = OnceLock::new();
    REGISTRY.get_or_init(build_std_module_registry)
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::{EMBEDDED_STD_MODULES, RUNTIME_STD_MODULES, StdModuleKind, std_module_registry};

    #[test]
    fn registry_module_lists_are_unique_and_have_registered_parents() {
        let listed: HashSet<&str> = RUNTIME_STD_MODULES
            .iter()
            .chain(EMBEDDED_STD_MODULES)
            .copied()
            .collect();
        assert_eq!(
            listed.len(),
            RUNTIME_STD_MODULES.len() + EMBEDDED_STD_MODULES.len(),
            "stdlib module lists contain a duplicate"
        );

        let registry = std_module_registry();
        assert_eq!(
            registry.len(),
            listed.len(),
            "registry contains stale entries"
        );
        for name in &listed {
            assert!(
                registry.contains_key(name),
                "missing stdlib registry entry: {name}"
            );
            let mut parent = *name;
            while let Some((candidate, _)) = parent.rsplit_once('.') {
                if candidate == "std" {
                    break;
                }
                assert!(
                    registry.contains_key(candidate),
                    "nested stdlib module {name} has no registered parent {candidate}"
                );
                parent = candidate;
            }
        }
    }

    #[test]
    fn embedded_registry_matches_sources_and_only_virtual_parents() {
        let source_names: HashSet<&str> = crate::embedded_std::embedded_std_sources()
            .keys()
            .filter(|name| name.starts_with("std."))
            .map(String::as_str)
            .collect();

        let registry = std_module_registry();
        for name in &source_names {
            assert_eq!(
                registry.get(name).map(|module| module.kind),
                Some(StdModuleKind::Embedded),
                "embedded source is missing from the registry: {name}"
            );
        }

        for name in EMBEDDED_STD_MODULES {
            if source_names.contains(name) {
                continue;
            }
            assert!(
                source_names
                    .iter()
                    .any(|source| source.starts_with(&format!("{name}."))),
                "registry entry {name} has no embedded source or child source"
            );
        }
    }

    #[test]
    fn every_embedded_module_is_registered_as_embedded() {
        let registry = std_module_registry();
        for name in EMBEDDED_STD_MODULES {
            let module = registry
                .get(name)
                .unwrap_or_else(|| panic!("missing std module registry entry: {name}"));
            assert_eq!(
                module.kind,
                StdModuleKind::Embedded,
                "wrong kind for {name}"
            );
        }
    }

    #[test]
    fn every_embedded_source_is_registered_as_embedded() {
        let registry = std_module_registry();
        for name in crate::embedded_std::embedded_std_sources()
            .keys()
            .filter(|name| name.starts_with("std."))
        {
            let module = registry
                .get(name.as_str())
                .unwrap_or_else(|| panic!("missing embedded std module registry entry: {name}"));
            assert_eq!(
                module.kind,
                StdModuleKind::Embedded,
                "wrong kind for embedded source {name}"
            );
        }
    }

    #[test]
    fn filesystem_operations_are_not_exposed_through_io() {
        assert!(crate::semantics::stdlib::lookup_stdlib_item("io.read_file").is_none());
        assert!(crate::semantics::stdlib::lookup_stdlib_item("io.cwd").is_none());
        assert!(crate::semantics::stdlib::lookup_stdlib_item("fs.read_file").is_some());
        assert!(crate::semantics::stdlib::lookup_stdlib_item("fs.cwd").is_some());
    }

    #[test]
    fn assert_is_a_global_builtin_and_not_a_stdlib_module() {
        let signature = crate::semantics::stdlib::BUILT_IN_FUNCTIONS
            .get("assert")
            .expect("assert must be registered as a global builtin");
        assert_eq!(signature.params.len(), 2);
        assert_eq!(
            signature.params[0],
            crate::semantics::Type::Primitive(crate::ast::PrimitiveType::Bool)
        );
        assert_eq!(
            signature.params[1],
            crate::semantics::Type::Primitive(crate::ast::PrimitiveType::Str)
        );
        assert!(crate::semantics::stdlib::lookup_stdlib_item("assert").is_none());
        assert!(crate::semantics::stdlib::lookup_stdlib_item("check.equal").is_none());
    }
}
