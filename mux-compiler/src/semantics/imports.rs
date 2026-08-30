use super::{BuiltInSig, MethodSig, SemanticAnalyzer, SemanticError, Symbol, SymbolKind, Type};
use crate::ast::{AstNode, ImportSpec, PrimitiveType, StatementKind, StatementNode};
use crate::diagnostic::DiagnosticCode;
use crate::diagnostic::Files;
use crate::lexer::Span;
use crate::semantics::std_registry::{StdModuleKind, std_module_registry};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

impl SemanticAnalyzer {
    // Add module as namespace (import logger as log)
    pub(super) fn add_module_namespace(
        &mut self,
        namespace: &str,
        symbols: std::collections::HashMap<String, Symbol>,
        module_path: &str,
        span: Span,
    ) -> Result<(), SemanticError> {
        // Mangle function names in the symbols before storing
        let module_name_for_mangling = crate::semantics::mangle_module_path(module_path);
        let mut mangled_symbols = std::collections::HashMap::new();

        for (name, symbol) in symbols {
            let mut mangled_symbol = symbol.clone();
            // Set llvm_name for functions and module constants (both are emitted
            // under module!name so same-named symbols in two modules differ)
            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Constant) {
                mangled_symbol.llvm_name = Some(format!("{module_name_for_mangling}!{name}"));
            }
            mangled_symbols.insert(name, mangled_symbol);
        }

        // Store module symbols for namespaced access
        self.imported_symbols
            .insert(namespace.to_string(), mangled_symbols);

        // Add module symbol to symbol table
        self.symbol_table.add_symbol(
            namespace,
            Symbol {
                kind: SymbolKind::Import,
                span,
                type_: Some(Type::Module(namespace.to_string())),
                interfaces: std::collections::HashMap::new(),
                methods: std::collections::HashMap::new(),
                fields: std::collections::HashMap::new(),
                type_params: Vec::new(),
                original_name: None,
                llvm_name: None,
                default_param_count: 0,
                variants: None,
            },
        )?;

        Ok(())
    }

    pub(super) fn filter_module_export_symbols(
        &self,
        symbols: &HashMap<String, Symbol>,
    ) -> HashMap<String, Symbol> {
        symbols
            .iter()
            .filter(|(_, symbol)| {
                matches!(
                    symbol.kind,
                    SymbolKind::Function
                        | SymbolKind::Class
                        | SymbolKind::Interface
                        | SymbolKind::Enum
                        | SymbolKind::Constant
                )
            })
            .map(|(name, symbol)| (name.clone(), symbol.clone()))
            .collect()
    }

    fn collect_declared_module_symbols(
        &self,
        module_nodes: &[AstNode],
        module_analyzer: &SemanticAnalyzer,
    ) -> HashMap<String, Symbol> {
        let global_symbols = module_analyzer.symbol_table.global_scope_symbols();
        let mut declared: HashMap<String, Symbol> = module_nodes
            .iter()
            .filter_map(Self::declared_symbol_name)
            .filter_map(|symbol_name| {
                global_symbols
                    .get(symbol_name)
                    .cloned()
                    .map(|symbol| (symbol_name.to_string(), symbol))
            })
            .collect();

        let interfaces = Self::implemented_interfaces(&declared, &global_symbols);
        declared.extend(interfaces);
        self.filter_module_export_symbols(&declared)
    }

    /// Carry along the interfaces the declared types implement.
    ///
    /// A module's exports are the symbols it DECLARES, which excludes an
    /// interface it merely imported - so `std.dsa.graph` exported `Graph` but
    /// not the `Collection` that `Graph` implements, and importing the type
    /// alone left codegen with no interface to build (#391).
    ///
    /// The interface is taken from the module's own scope, which is where its
    /// import of `std.dsa.collection` put it, so this reuses a resolution that
    /// already happened rather than performing a second one.
    fn implemented_interfaces(
        declared: &HashMap<String, Symbol>,
        module_scope: &HashMap<String, Symbol>,
    ) -> HashMap<String, Symbol> {
        let mut found = HashMap::new();
        for symbol in declared.values() {
            for interface_name in symbol.interfaces.keys() {
                if declared.contains_key(interface_name) {
                    continue;
                }
                if let Some(interface) = module_scope.get(interface_name) {
                    found.insert(interface_name.clone(), interface.clone());
                }
            }
        }
        found
    }

    fn declared_symbol_name(node: &AstNode) -> Option<&str> {
        match node {
            AstNode::Function(func) => Some(func.name.as_str()),
            AstNode::Class { name, .. }
            | AstNode::Interface { name, .. }
            | AstNode::Enum { name, .. } => Some(name.as_str()),
            AstNode::Statement(stmt) => Self::declared_statement_symbol_name(stmt),
        }
    }

    fn declared_statement_symbol_name(stmt: &StatementNode) -> Option<&str> {
        match &stmt.kind {
            StatementKind::AutoDecl(name, _, _)
            | StatementKind::TypedDecl(name, _, _)
            | StatementKind::ConstDecl(name, _, _) => Some(name.as_str()),
            StatementKind::Function(func) => Some(func.name.as_str()),
            _ => None,
        }
    }

    /// Fold everything an imported module's throwaway analyzer discovered back
    /// into this one.
    ///
    /// That analyzer is the only thing that resolved the imported module's own
    /// imports, so its module table is the sole record of them. Without this, a
    /// module reachable only transitively (main imports a, which imports b)
    /// never reaches codegen: its globals are never declared, and referencing
    /// them fails with "Undefined variable" even though semantic analysis
    /// resolved them. Existing AST entries win, so a module already registered
    /// keeps the AST analyzed in its own right.
    ///
    /// Dependency order matters as well: main calls the init functions in
    /// sequence, and a module's constants must be initialized before any module
    /// that reads them. The imported module's own dependencies are appended
    /// here, ahead of the module itself, so they initialize first.
    fn absorb_module_analyzer(&mut self, module_analyzer: &mut SemanticAnalyzer) {
        for (path, nodes) in std::mem::take(&mut module_analyzer.all_module_asts) {
            self.all_module_asts.entry(path).or_insert(nodes);
        }

        for dependency in std::mem::take(&mut module_analyzer.module_dependencies) {
            if !self.module_dependencies.contains(&dependency) {
                self.module_dependencies.push(dependency);
            }
        }
    }

    fn analyze_imported_module(
        &mut self,
        module_nodes: &[AstNode],
        module_path: &str,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
        span: Span,
    ) -> Result<HashMap<String, Symbol>, SemanticError> {
        let mut module_analyzer = SemanticAnalyzer::new_with_resolver(resolver.clone());
        module_analyzer.set_imported_module();
        module_analyzer.set_current_file(std::path::PathBuf::from(
            module_path.replace('.', "/") + ".mux",
        ));
        if let Some(file_id) = resolver.borrow().file_id_for_module(module_path) {
            module_analyzer.set_current_file_id(file_id);
        }
        let errors = module_analyzer.analyze(module_nodes, Some(files));
        self.imported_errors
            .extend(module_analyzer.take_imported_errors());
        // Codegen queries a single, shared analyzer for expression types
        // (e.g. to disambiguate an empty `{}` literal as a map vs a set), but
        // each imported module is analyzed with its own throwaway analyzer
        let has_errors = errors
            .iter()
            .any(|error| error.code.level() == crate::diagnostic::Level::Error);
        self.imported_errors.extend(errors.iter().cloned());
        if has_errors {
            resolver.borrow_mut().finish_import(module_path);
            let error_messages: Vec<String> =
                errors.iter().map(|e| e.message.to_string()).collect();
            return Err(SemanticError::with_help(
                DiagnosticCode::ImportFailure,
                format!("Errors in imported module '{module_path}'"),
                span,
                format!(
                    "Fix the following errors in '{}':\n  {}",
                    module_path,
                    error_messages.join("\n  ")
                ),
            ));
        }
        self.absorb_module_analyzer(&mut module_analyzer);

        Ok(self.collect_declared_module_symbols(module_nodes, &module_analyzer))
    }

    fn mangle_and_import_module_symbols(
        &mut self,
        module_symbols: &HashMap<String, Symbol>,
        module_path: &str,
    ) -> Result<(), SemanticError> {
        let module_name_for_mangling = crate::semantics::mangle_module_path(module_path);
        for (name, symbol) in module_symbols {
            let name_str = name.as_str();
            // Builtin *functions* (print, read_line, range, some, none, ok, err)
            // keep unmangled names. Constants are never builtins here, so a
            // module constant that merely starts with one of these prefixes
            // (e.g. `err_code`, `print_width`) must not be skipped.
            let is_unmangled_builtin_function = matches!(symbol.kind, SymbolKind::Function)
                && (name_str.starts_with("print")
                    || name_str.starts_with("read_line")
                    || name_str.starts_with("range")
                    || name_str.starts_with("some")
                    || name_str.starts_with("none")
                    || name_str.starts_with("ok")
                    || name_str.starts_with("err"));

            if !is_unmangled_builtin_function && !self.symbol_table.all_symbols.contains_key(name) {
                let mut mangled_symbol = symbol.clone();
                if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Constant) {
                    mangled_symbol.llvm_name = Some(format!("{module_name_for_mangling}!{name}"));
                }
                self.symbol_table
                    .all_symbols
                    .insert(name.clone(), mangled_symbol);
            }
        }

        Ok(())
    }

    fn handle_import_spec(
        &mut self,
        spec: &ImportSpec,
        module_symbols: &HashMap<String, Symbol>,
        module_path: &str,
        span: Span,
    ) -> Result<(), SemanticError> {
        match spec {
            ImportSpec::Module { alias } => {
                if let Some(namespace) = alias {
                    self.add_module_namespace(
                        namespace,
                        module_symbols.clone(),
                        module_path,
                        span,
                    )?;
                } else if module_path.contains('.') {
                    let name = module_path
                        .split('.')
                        .next_back()
                        .expect("module path should include at least one segment");
                    self.add_module_namespace(name, module_symbols.clone(), module_path, span)?;
                }
            }
            ImportSpec::Item { item, alias } => {
                let symbol_name = alias.as_ref().unwrap_or(item);
                self.import_single_symbol(module_symbols, item, symbol_name, module_path, span)?;
            }
            ImportSpec::Items { items } => {
                for (item, alias) in items {
                    let symbol_name = alias.as_ref().unwrap_or(item);
                    self.import_single_symbol(
                        module_symbols,
                        item,
                        symbol_name,
                        module_path,
                        span,
                    )?;
                }
            }
            ImportSpec::Wildcard => {
                self.import_all_symbols(module_symbols, module_path, span)?;
            }
        }
        Ok(())
    }

    pub(super) fn import_module_from_resolver(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        let resolver = self.module_resolver.clone().ok_or_else(|| {
            SemanticError::new(
                DiagnosticCode::ImportFailure,
                "Module resolver not available",
                span,
            )
        })?;

        let (has_file, has_directory) = resolver.borrow().check_module_path(module_path);
        if has_file && has_directory {
            return Err(SemanticError::with_help(
                DiagnosticCode::ImportFailure,
                format!("Ambiguous import: '{module_path}'"),
                span,
                format!(
                    "Both {}.mux and {}/ directory exist. Please remove one.",
                    module_path.replace('.', "/"),
                    module_path.replace('.', "/")
                ),
            ));
        }

        if has_directory {
            self.handle_directory_import(module_path, spec, span, resolver, files)?;
            return Ok(());
        }

        let module_nodes = resolver
            .borrow_mut()
            .resolve_import_path(module_path, self.current_file.as_deref(), files)
            .map_err(|e| {
                SemanticError::with_help(
                    e.code,
                    format!("Failed to import module '{module_path}'"),
                    span,
                    e.message,
                )
            })?;

        let module_symbols =
            self.analyze_imported_module(&module_nodes, module_path, &resolver, files, span)?;
        self.mangle_and_import_module_symbols(&module_symbols, module_path)?;

        self.handle_import_spec(spec, &module_symbols, module_path, span)?;

        resolver
            .borrow_mut()
            .cache_module(module_path, module_nodes.clone());
        resolver.borrow_mut().finish_import(module_path);
        self.all_module_asts
            .insert(module_path.to_string(), module_nodes);

        if !self.module_dependencies.contains(&module_path.to_string()) {
            self.module_dependencies.push(module_path.to_string());
        }

        Ok(())
    }

    // Handle directory-based module import (e.g., import utils where utils/ is a directory)
    fn resolve_and_register_submodule(
        &mut self,
        submodule_path: &str,
        namespace: &str,
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<Symbol, SemanticError> {
        let submodule_nodes = resolver
            .borrow_mut()
            .resolve_import_path(submodule_path, self.current_file.as_deref(), files)
            .map_err(|e| {
                SemanticError::with_help(
                    e.code,
                    format!("Failed to import submodule '{submodule_path}'"),
                    span,
                    e.message,
                )
            })?;

        let mut submodule_analyzer = SemanticAnalyzer::new_with_resolver(resolver.clone());
        submodule_analyzer.set_imported_module();
        submodule_analyzer.set_current_file(std::path::PathBuf::from(
            submodule_path.replace('.', "/") + ".mux",
        ));
        if let Some(file_id) = resolver.borrow().file_id_for_module(submodule_path) {
            submodule_analyzer.set_current_file_id(file_id);
        }
        let errors = submodule_analyzer.analyze(&submodule_nodes, Some(files));
        self.imported_errors
            .extend(submodule_analyzer.take_imported_errors());
        let has_errors = errors
            .iter()
            .any(|error| error.code.level() == crate::diagnostic::Level::Error);
        self.imported_errors.extend(errors.iter().cloned());
        if has_errors {
            resolver.borrow_mut().finish_import(submodule_path);
            let error_messages: Vec<String> =
                errors.iter().map(|e| e.message.to_string()).collect();
            return Err(SemanticError::with_help(
                DiagnosticCode::ImportFailure,
                format!("Errors in submodule '{submodule_path}'"),
                span,
                format!(
                    "Fix the following errors in '{}':\n  {}",
                    submodule_path,
                    error_messages.join("\n  ")
                ),
            ));
        }

        let submodule_symbols =
            self.collect_declared_module_symbols(&submodule_nodes, &submodule_analyzer);
        self.mangle_and_import_module_symbols(&submodule_symbols, submodule_path)?;

        let mangled_submodule_symbols =
            self.mangle_module_symbols(&submodule_symbols, submodule_path);
        self.imported_symbols
            .insert(namespace.to_string(), mangled_submodule_symbols);

        let symbol = self.make_module_symbol(namespace, span);

        resolver
            .borrow_mut()
            .cache_module(submodule_path, submodule_nodes.clone());
        resolver.borrow_mut().finish_import(submodule_path);

        self.all_module_asts
            .insert(submodule_path.to_string(), submodule_nodes);

        if !self
            .module_dependencies
            .contains(&submodule_path.to_string())
        {
            self.module_dependencies.push(submodule_path.to_string());
        }

        Ok(symbol)
    }

    pub(super) fn handle_directory_import(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        resolver: Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        let submodules = self.get_directory_submodules(module_path, span, &resolver)?;
        match spec {
            ImportSpec::Module { alias } => self.import_directory_as_module(
                module_path,
                alias.as_deref(),
                &submodules,
                span,
                &resolver,
                files,
            )?,
            ImportSpec::Item { item, alias } => {
                self.ensure_directory_submodule_exists(module_path, item, &submodules, span)?;
                self.import_directory_single_item(
                    module_path,
                    item,
                    alias.as_deref(),
                    span,
                    &resolver,
                    files,
                )?;
            }
            ImportSpec::Items { items } => self.import_directory_items(
                module_path,
                items,
                &submodules,
                span,
                &resolver,
                files,
            )?,
            ImportSpec::Wildcard => {
                self.import_directory_wildcard(module_path, &submodules, span, &resolver, files)?;
            }
        }
        Ok(())
    }

    fn get_directory_submodules(
        &self,
        module_path: &str,
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
    ) -> Result<Vec<String>, SemanticError> {
        resolver.borrow().get_submodules(module_path).map_err(|e| {
            SemanticError::new(
                DiagnosticCode::ImportFailure,
                format!("Failed to get submodules: {e}"),
                span,
            )
        })
    }

    fn ensure_directory_submodule_exists(
        &self,
        module_path: &str,
        submodule_name: &str,
        submodules: &[String],
        span: Span,
    ) -> Result<(), SemanticError> {
        if submodules.contains(&submodule_name.to_string()) {
            return Ok(());
        }
        Err(SemanticError::with_help(
            DiagnosticCode::ImportFailure,
            format!("Submodule '{submodule_name}' not found in '{module_path}'"),
            span,
            format!("Available submodules: {}", submodules.join(", ")),
        ))
    }

    fn import_directory_as_module(
        &mut self,
        module_path: &str,
        alias: Option<&str>,
        submodules: &[String],
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        let namespace = alias.unwrap_or(module_path);
        let mut module_symbols = std::collections::HashMap::new();
        for submodule_name in submodules {
            let submodule_path = format!("{module_path}.{submodule_name}");
            let symbol = self.resolve_and_register_submodule(
                &submodule_path,
                submodule_name,
                span,
                resolver,
                files,
            )?;
            module_symbols.insert(submodule_name.to_string(), symbol);
        }
        self.imported_symbols
            .insert(namespace.to_string(), module_symbols);
        self.symbol_table.add_symbol(
            namespace,
            Symbol {
                kind: SymbolKind::Import,
                span,
                type_: Some(Type::Module(namespace.to_string())),
                interfaces: std::collections::HashMap::new(),
                methods: std::collections::HashMap::new(),
                fields: std::collections::HashMap::new(),
                type_params: Vec::new(),
                original_name: None,
                llvm_name: None,
                default_param_count: 0,
                variants: None,
            },
        )?;
        Ok(())
    }

    fn import_directory_single_item(
        &mut self,
        module_path: &str,
        item: &str,
        alias: Option<&str>,
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        let submodule_path = format!("{module_path}.{item}");
        let namespace = alias.unwrap_or(item);
        let symbol =
            self.resolve_and_register_submodule(&submodule_path, namespace, span, resolver, files)?;
        self.symbol_table.add_symbol(namespace, symbol)?;
        Ok(())
    }

    fn import_directory_items(
        &mut self,
        module_path: &str,
        items: &[(String, Option<String>)],
        submodules: &[String],
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        for (item, alias) in items {
            self.ensure_directory_submodule_exists(module_path, item, submodules, span)?;
            self.import_directory_single_item(
                module_path,
                item,
                alias.as_deref(),
                span,
                resolver,
                files,
            )?;
        }
        Ok(())
    }

    fn import_directory_wildcard(
        &mut self,
        module_path: &str,
        submodules: &[String],
        span: Span,
        resolver: &Rc<RefCell<crate::module_resolver::ModuleResolver>>,
        files: &mut Files,
    ) -> Result<(), SemanticError> {
        for submodule_name in submodules {
            let submodule_path = format!("{module_path}.{submodule_name}");
            let symbol = self.resolve_and_register_submodule(
                &submodule_path,
                submodule_name,
                span,
                resolver,
                files,
            )?;
            self.symbol_table.add_symbol(submodule_name, symbol)?;
        }
        Ok(())
    }

    // Helper method to mangle module symbols
    fn mangle_module_symbols(
        &self,
        symbols: &std::collections::HashMap<String, Symbol>,
        module_path: &str,
    ) -> std::collections::HashMap<String, Symbol> {
        let module_name_for_mangling = crate::semantics::mangle_module_path(module_path);
        let mut mangled_symbols = std::collections::HashMap::new();

        for (name, symbol) in symbols {
            let mut mangled_symbol = symbol.clone();
            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Constant) {
                mangled_symbol.llvm_name = Some(format!("{module_name_for_mangling}!{name}"));
            }
            mangled_symbols.insert(name.clone(), mangled_symbol);
        }

        mangled_symbols
    }

    // Import single symbol (import logger.log)
    pub(super) fn import_single_symbol(
        &mut self,
        module_symbols: &std::collections::HashMap<String, Symbol>,
        item_name: &str,
        local_name: &str,
        module_path: &str,
        span: Span,
    ) -> Result<(), SemanticError> {
        let symbol = module_symbols.get(item_name).ok_or_else(|| {
            let available: Vec<&String> = module_symbols.keys().collect();
            if available.is_empty() {
                SemanticError::new(
                    DiagnosticCode::ImportFailure,
                    format!("Symbol '{item_name}' not found in module '{module_path}'"),
                    span,
                )
            } else {
                SemanticError::with_help(
                    DiagnosticCode::ImportFailure,
                    format!("Symbol '{item_name}' not found in module '{module_path}'"),
                    span,
                    format!(
                        "Available symbols: {}",
                        available
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                )
            }
        })?;

        // Clone the symbol and set original_name/llvm_name if needed
        let mut imported_symbol = symbol.clone();

        // Set original_name if it's an alias
        if item_name != local_name {
            imported_symbol.original_name = Some(item_name.to_string());
        }

        // Set llvm_name for functions and module constants (mangled with module path)
        if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Constant) {
            let module_name_for_mangling = crate::semantics::mangle_module_path(module_path);
            imported_symbol.llvm_name = Some(format!("{module_name_for_mangling}!{item_name}"));
        }

        self.add_import_symbol_if_absent(local_name, imported_symbol)?;

        // Bring in the interfaces this type implements. Importing a type used
        // to leave them behind, so `import std.dsa.graph.Graph` alone left
        // `Collection` unknown and codegen had no interface to build - reported
        // first as an internal compiler error, then as a diagnostic telling the
        // user to add an import they never mentioned. Neither is a good answer
        // when the requirement is knowable from the type itself (#391).
        self.import_implemented_interfaces(symbol, item_name, module_path, module_symbols, span)?;
        Ok(())
    }

    /// Import the interfaces `symbol` implements, from the same module.
    ///
    /// Only what the module exports is reachable here, which is why
    /// `collect_declared_module_symbols` carries an implemented interface along
    /// with the type it belongs to. That covers the case worth covering: an
    /// interface the module itself can see.
    ///
    /// One the module cannot supply is recorded rather than reported, because
    /// the interface may still arrive from a later import. The deferred check
    /// in `verify_pending_interface_imports` owns that diagnostic and runs once
    /// every import has resolved, so it never depends on the order they were
    /// written - and it is what keeps an unresolved interface from reaching
    /// codegen, where the failure was an internal compiler error.
    fn import_implemented_interfaces(
        &mut self,
        symbol: &Symbol,
        item_name: &str,
        module_path: &str,
        module_symbols: &std::collections::HashMap<String, Symbol>,
        span: Span,
    ) -> Result<(), SemanticError> {
        if !matches!(symbol.kind, SymbolKind::Class | SymbolKind::Enum) {
            return Ok(());
        }

        let names: Vec<String> = symbol.interfaces.keys().cloned().collect();
        for interface_name in names {
            if self.symbol_table.get_cloned(&interface_name).is_some() {
                continue;
            }
            match module_symbols.get(&interface_name) {
                Some(interface) => {
                    self.add_import_symbol_if_absent(&interface_name, interface.clone())?;
                }
                None => self.pending_interface_imports.push((
                    item_name.to_string(),
                    interface_name,
                    module_path.to_string(),
                    span,
                )),
            }
        }
        Ok(())
    }

    /// Report any imported type whose interface never came into scope.
    ///
    /// Deferred until every import is resolved, so `import ...graph.Graph`
    /// followed by `import ...collection.Collection` is accepted - checking at
    /// import time would reject the first line for something the second line
    /// supplies.
    pub(super) fn verify_pending_interface_imports(&mut self) {
        for (item_name, interface_name, module_path, span) in
            std::mem::take(&mut self.pending_interface_imports)
        {
            if self.symbol_table.get_cloned(&interface_name).is_some() {
                continue;
            }
            // The interface sits beside the type's module rather than inside it
            // (`std.dsa.collection.Collection` next to `std.dsa.graph`), so the
            // useful wildcard is the PARENT - `std.dsa.*`, not `std.*`.
            let parent = module_path
                .rsplit_once('.')
                .map_or(module_path.as_str(), |(head, _)| head);
            self.errors.push(SemanticError::with_help(
                DiagnosticCode::ImportFailure,
                format!(
                    "'{item_name}' implements interface '{interface_name}', which is not imported"
                ),
                span,
                format!(
                    "importing a type does not import the interfaces it implements. \
                     Import '{interface_name}' as well, or import the whole module with 'import {parent}.*'"
                ),
            ));
        }
    }

    fn add_import_symbol_if_absent(
        &mut self,
        name: &str,
        symbol: Symbol,
    ) -> Result<(), SemanticError> {
        if self.symbol_table.get_cloned(name).is_some() {
            return Ok(());
        }
        self.symbol_table.add_imported_symbol(name, symbol)
    }

    // Import all symbols (import logger.*)
    pub(super) fn import_all_symbols(
        &mut self,
        module_symbols: &std::collections::HashMap<String, Symbol>,
        module_path: &str,
        _span: Span,
    ) -> Result<(), SemanticError> {
        let module_name_for_mangling = crate::semantics::mangle_module_path(module_path);

        for (name, symbol) in module_symbols {
            let mut imported_symbol = symbol.clone();

            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Constant) {
                imported_symbol.llvm_name = Some(format!("{module_name_for_mangling}!{name}"));
            }

            self.add_import_symbol_if_absent(name, imported_symbol)?;
        }
        Ok(())
    }

    /// Register a single built-in function into the symbol table from its signature.
    pub(super) fn register_builtin_function(&mut self, name: &str, sig: &BuiltInSig, span: Span) {
        let _ = self.symbol_table.add_symbol(
            name,
            Self::make_symbol(
                SymbolKind::Function,
                span,
                Some(Type::Function {
                    params: sig.params.clone(),
                    returns: Box::new(sig.return_type.clone()),
                    default_count: 0,
                }),
            ),
        );
    }

    /// Register all built-in functions whose names start with the given prefix.
    fn register_builtin_functions_with_prefix(&mut self, prefix: &str, span: Span) {
        let matching: Vec<_> = crate::semantics::stdlib::BUILT_IN_FUNCTIONS
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect();
        for (func_name, sig) in matching {
            self.register_builtin_function(&func_name, &sig, span);
        }
    }

    fn import_std_item_with_resolver_check(
        &mut self,
        item: &str,
        alias: &Option<String>,
        span: Span,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        let full_module_path = format!("std.{item}");
        if let Some(resolver) = &self.module_resolver {
            let (has_file, has_directory) = resolver.borrow().check_module_path(&full_module_path);
            if has_file || has_directory {
                let files = files.ok_or_else(|| {
                    SemanticError::new(
                        DiagnosticCode::ImportFailure,
                        "Files registry must be available for std imports",
                        span,
                    )
                })?;
                return self.import_module_from_resolver(
                    &full_module_path,
                    &ImportSpec::Module {
                        alias: alias.clone(),
                    },
                    span,
                    files,
                );
            }
        }
        self.import_stdlib_module(
            item,
            &ImportSpec::Module {
                alias: alias.clone(),
            },
            span,
        )
    }

    fn handle_std_import_with_std_prefix(
        &mut self,
        spec: &ImportSpec,
        span: Span,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        let mut files = files;
        match spec {
            ImportSpec::Item { item, alias } => {
                self.import_std_item_with_resolver_check(item, alias, span, files.as_deref_mut())
            }
            ImportSpec::Items { items } => {
                for (item, alias) in items {
                    self.import_std_item_with_resolver_check(
                        item,
                        alias,
                        span,
                        files.as_deref_mut(),
                    )?;
                }
                Ok(())
            }
            _ => self.handle_std_import_all(spec, span),
        }
    }

    fn files_for_std_import(
        files: Option<&mut Files>,
        span: Span,
    ) -> Result<&mut Files, SemanticError> {
        files.ok_or_else(|| {
            SemanticError::new(
                DiagnosticCode::InvalidOperation,
                "Files registry must be available",
                span,
            )
        })
    }

    fn import_registry_std_module(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        files: Option<&mut Files>,
        def: &crate::semantics::std_registry::StdModuleDef,
    ) -> Result<(), SemanticError> {
        match def.kind {
            StdModuleKind::RuntimeBacked => {
                let module_name = module_path.strip_prefix("std.").unwrap_or(module_path);
                self.import_stdlib_module(module_name, spec, span)
            }
            StdModuleKind::Embedded => {
                let files = Self::files_for_std_import(files, span)?;
                self.import_module_from_resolver(module_path, spec, span, files)
            }
        }
    }

    fn import_nested_std_module_if_present(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        files: Option<&mut Files>,
        registry: &std::collections::HashMap<
            &'static str,
            crate::semantics::std_registry::StdModuleDef,
        >,
    ) -> Result<Option<()>, SemanticError> {
        let Some(tail) = module_path.strip_prefix("std.") else {
            return Ok(None);
        };

        let Some(parent) = tail.split('.').next() else {
            return Ok(None);
        };

        let parent_path = format!("std.{parent}");
        if !registry.contains_key(parent_path.as_str()) {
            return Ok(None);
        }

        let Some(def) = registry.get(module_path) else {
            return Ok(None);
        };

        match def.kind {
            StdModuleKind::RuntimeBacked => {
                self.import_stdlib_module(tail, spec, span)?;
            }
            StdModuleKind::Embedded => {
                let files = Self::files_for_std_import(files, span)?;
                self.import_module_from_resolver(tail, spec, span, files)?;
            }
        }

        Ok(Some(()))
    }

    pub(super) fn handle_std_import(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
        files: Option<&mut Files>,
    ) -> Result<(), SemanticError> {
        if module_path == "std" {
            return self.handle_std_import_with_std_prefix(spec, span, files);
        }

        if let Some(resolver) = &self.module_resolver {
            let (has_file, has_directory) = resolver.borrow().check_module_path(module_path);
            if has_file || has_directory {
                let files = Self::files_for_std_import(files, span)?;
                return self.import_module_from_resolver(module_path, spec, span, files);
            }
        }

        let registry = std_module_registry();
        if let Some(def) = registry.get(module_path) {
            return self.import_registry_std_module(module_path, spec, span, files, def);
        }

        if self
            .import_nested_std_module_if_present(module_path, spec, span, files, registry)?
            .is_some()
        {
            return Ok(());
        }

        match module_path {
            "std" | "stdlib" => self.handle_std_import_all(spec, span),
            _ => self.handle_non_stdlib_import(module_path, spec, span),
        }
    }

    fn handle_std_import_all(
        &mut self,
        spec: &ImportSpec,
        span: Span,
    ) -> Result<(), SemanticError> {
        let registry = std_module_registry();

        match spec {
            ImportSpec::Module { alias } => {
                self.import_all_std_as_namespace(alias.as_deref(), span, registry)?;
            }
            ImportSpec::Wildcard => self.import_all_std_wildcard(span)?,
            ImportSpec::Items { items } => {
                for (item, alias) in items {
                    self.import_stdlib_module(
                        item,
                        &ImportSpec::Module {
                            alias: alias.clone(),
                        },
                        span,
                    )?;
                }
            }
            ImportSpec::Item { item, alias } => {
                self.import_stdlib_module(
                    item,
                    &ImportSpec::Module {
                        alias: alias.clone(),
                    },
                    span,
                )?;
            }
        }
        Ok(())
    }

    fn import_all_std_as_namespace(
        &mut self,
        alias: Option<&str>,
        span: Span,
        registry: &std::collections::HashMap<
            &'static str,
            crate::semantics::std_registry::StdModuleDef,
        >,
    ) -> Result<(), SemanticError> {
        let namespace = alias.unwrap_or("std");
        let namespace_symbols: std::collections::HashMap<String, Symbol> = registry
            .keys()
            .filter_map(|m| {
                let rest = m.strip_prefix("std.")?;
                if rest.contains('.') {
                    None
                } else {
                    Some((rest.to_string(), self.make_module_symbol(m, span)))
                }
            })
            .collect();

        for module_path in registry.keys() {
            let short_name = module_path.strip_prefix("std.").unwrap_or(module_path);
            if short_name.contains('.') {
                continue;
            }

            let module_symbols = self.collect_stdlib_module_symbols(short_name, span);
            self.imported_symbols
                .insert(short_name.to_string(), module_symbols.clone());
            self.imported_symbols
                .insert(module_path.to_string(), module_symbols);
            self.inject_nested_stdlib_children(short_name, span);
        }

        self.imported_symbols
            .entry(namespace.to_string())
            .or_insert(namespace_symbols);
        self.add_import_symbol_if_absent(namespace, self.make_module_symbol(namespace, span))?;
        Ok(())
    }

    fn import_all_std_wildcard(&mut self, span: Span) -> Result<(), SemanticError> {
        for (key, item) in crate::semantics::stdlib::all_stdlib_items() {
            if let Some(item_name) = key.find('.').map(|i| &key[i + 1..]) {
                let symbol = crate::semantics::stdlib::stdlib_item_to_symbol(&item, span);
                self.add_import_symbol_if_absent(item_name, symbol)?;
            }
        }

        Ok(())
    }

    fn handle_non_stdlib_import(
        &mut self,
        module_path: &str,
        spec: &ImportSpec,
        span: Span,
    ) -> Result<(), SemanticError> {
        let module_name = module_path
            .split('.')
            .next_back()
            .expect("module path should have at least one component");

        match spec {
            ImportSpec::Module { alias } => {
                let symbol_name = alias
                    .as_ref()
                    .map_or(module_name, std::string::String::as_str);
                if let Some(sig) = self.get_builtin_sig(symbol_name).cloned() {
                    self.register_builtin_function(symbol_name, &sig, span);
                } else if symbol_name == "none" {
                    self.symbol_table.add_symbol(
                        symbol_name,
                        Self::make_symbol(
                            SymbolKind::Constant,
                            span,
                            Some(Type::Optional(Box::new(Type::Void))),
                        ),
                    )?;
                } else {
                    self.register_builtin_functions_with_prefix(&format!("{symbol_name}_"), span);
                }
            }
            ImportSpec::Item { item, alias } => {
                let symbol_name = alias.as_ref().unwrap_or(item);
                if let Some(sig) = self.get_builtin_sig(item).cloned() {
                    self.register_builtin_function(symbol_name, &sig, span);
                }
            }
            ImportSpec::Wildcard => {
                self.register_builtin_functions_with_prefix(&format!("{module_name}_"), span);
            }
            ImportSpec::Items { items } => {
                for (item, alias) in items {
                    let qualified_name = format!("{module_name}_{item}");
                    let symbol_name = alias.as_ref().unwrap_or(&qualified_name);
                    if let Some(sig) = self.get_builtin_sig(&qualified_name).cloned() {
                        self.register_builtin_function(symbol_name, &sig, span);
                    }
                }
            }
        }
        Ok(())
    }

    /// Import a stdlib module (e.g., "math", "random")
    fn import_stdlib_module(
        &mut self,
        module_name: &str,
        spec: &ImportSpec,
        span: Span,
    ) -> Result<(), SemanticError> {
        let module_symbols = self.collect_stdlib_module_symbols(module_name, span);
        // Inject any nested stdlib children declared for this parent
        self.inject_nested_stdlib_children(module_name, span);
        self.apply_stdlib_module_import_spec(module_name, spec, span, module_symbols)
    }

    pub(super) fn collect_stdlib_module_symbols(
        &self,
        module_name: &str,
        span: Span,
    ) -> std::collections::HashMap<String, Symbol> {
        let mut module_symbols = std::collections::HashMap::new();
        let prefixes = Self::stdlib_module_prefixes(module_name);

        for (key, item) in crate::semantics::stdlib::all_stdlib_items() {
            if let Some(item_name) = Self::stdlib_item_name_for_module(&key, &prefixes) {
                module_symbols.insert(
                    item_name,
                    crate::semantics::stdlib::stdlib_item_to_symbol(&item, span),
                );
            }
        }

        Self::inject_stdlib_module_special_symbols(module_name, span, &mut module_symbols);
        module_symbols
    }

    fn stdlib_module_prefixes(module_name: &str) -> Vec<String> {
        if !module_name.contains('.') {
            return vec![module_name.to_string()];
        }
        let mut prefixes = vec![module_name.to_string()];
        if let Some(last) = module_name.split('.').next_back() {
            prefixes.push(last.to_string());
        }
        prefixes
    }

    fn stdlib_item_name_for_module(key: &str, prefixes: &[String]) -> Option<String> {
        for prefix in prefixes {
            let pattern = format!("{prefix}.");
            if let Some(rest) = key.strip_prefix(&pattern)
                && !rest.contains('.')
            {
                return Some(rest.to_string());
            }
        }
        None
    }

    fn inject_stdlib_module_special_symbols(
        module_name: &str,
        span: Span,
        module_symbols: &mut std::collections::HashMap<String, Symbol>,
    ) {
        match module_name {
            "net" => {
                module_symbols.extend(crate::semantics::stdlib::net_module_class_symbols(span));
            }
            "sync" => {
                module_symbols.extend(crate::semantics::stdlib::sync_module_class_symbols(span));
            }
            "sql" => {
                module_symbols.extend(crate::semantics::stdlib::sql_module_class_symbols(span));
            }
            _ if module_name.ends_with(".json") => {
                module_symbols.insert("Json".to_string(), Self::make_json_symbol(span));
            }
            _ if module_name.ends_with(".csv") => {
                module_symbols.insert("Csv".to_string(), Self::make_csv_symbol(span));
            }
            _ => {}
        }
    }

    fn csv_headers_type() -> Type {
        Type::List(Box::new(Type::Primitive(PrimitiveType::Str)))
    }

    fn csv_rows_type() -> Type {
        Type::List(Box::new(Self::csv_headers_type()))
    }

    fn csv_fields() -> std::collections::HashMap<String, (Type, bool)> {
        let mut fields = std::collections::HashMap::new();
        fields.insert("headers".to_string(), (Self::csv_headers_type(), true));
        fields.insert("rows".to_string(), (Self::csv_rows_type(), true));
        fields
    }

    pub(super) fn make_csv_symbol(span: Span) -> Symbol {
        let mut methods = std::collections::HashMap::new();
        // The total render, as on Json (#405).
        methods.insert(
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            },
        );
        methods.insert(
            "stringify".to_string(),
            MethodSig {
                params: vec![],
                return_type: Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                ),
                is_static: false,
            },
        );
        Symbol {
            kind: SymbolKind::Class,
            span,
            type_: Some(Type::Named("Csv".to_string(), Vec::new())),
            interfaces: std::collections::HashMap::new(),
            methods,
            fields: Self::csv_fields(),
            type_params: Vec::new(),
            original_name: None,
            llvm_name: None,
            default_param_count: 0,
            variants: None,
        }
    }

    fn make_json_symbol(span: Span) -> Symbol {
        let mut methods = std::collections::HashMap::new();
        methods.insert(
            "stringify".to_string(),
            MethodSig {
                params: vec![Type::Optional(Box::new(Type::Primitive(
                    PrimitiveType::Int,
                )))],
                return_type: Type::Result(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                ),
                is_static: false,
            },
        );

        // Typed accessors, each returning `result<T, string>` - the error names
        // what was actually there, so "not an int" says whether it was a string,
        // a null, or something else. Without these `stringify` was the only way
        // to inspect a value, so a string field came back JSON-encoded with its
        // quotes and nothing could strip them (#392, #404).
        // The total render every other type has. `stringify` stays for the
        // configurable form - it takes an indent and returns a result, so it
        // cannot satisfy Stringable (#405).
        methods.insert(
            "to_string".to_string(),
            MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Str),
                is_static: false,
            },
        );

        let json = Type::Named("Json".to_string(), Vec::new());
        for (name, inner) in [
            ("as_string", Type::Primitive(PrimitiveType::Str)),
            ("as_int", Type::Primitive(PrimitiveType::Int)),
            ("as_float", Type::Primitive(PrimitiveType::Float)),
            ("as_bool", Type::Primitive(PrimitiveType::Bool)),
            ("as_list", Type::List(Box::new(json.clone()))),
            (
                "as_map",
                Type::Map(
                    Box::new(Type::Primitive(PrimitiveType::Str)),
                    Box::new(json.clone()),
                ),
            ),
        ] {
            methods.insert(
                name.to_string(),
                MethodSig {
                    params: vec![],
                    return_type: Type::Result(
                        Box::new(inner),
                        Box::new(Type::Primitive(PrimitiveType::Str)),
                    ),
                    is_static: false,
                },
            );
        }

        // Not an accessor: a JSON null is a kind, not an absent value, so this
        // answers a question rather than yielding one.
        methods.insert(
            "is_null".to_string(),
            MethodSig {
                params: vec![],
                return_type: Type::Primitive(PrimitiveType::Bool),
                is_static: false,
            },
        );

        Symbol {
            kind: SymbolKind::Class,
            span,
            type_: Some(Type::Named("Json".to_string(), Vec::new())),
            interfaces: std::collections::HashMap::new(),
            methods,
            fields: std::collections::HashMap::new(),
            type_params: Vec::new(),
            original_name: None,
            llvm_name: None,
            default_param_count: 0,
            variants: None,
        }
    }

    fn apply_stdlib_module_import_spec(
        &mut self,
        module_name: &str,
        spec: &ImportSpec,
        span: Span,
        module_symbols: std::collections::HashMap<String, Symbol>,
    ) -> Result<(), SemanticError> {
        match spec {
            ImportSpec::Module { alias } => {
                let namespace = alias.as_deref().map_or_else(
                    || {
                        // If importing a nested module like "data.json" prefer the
                        // short child name "json" as the namespace so callers can
                        // reference `json.parse` without an alias.
                        if module_name.contains('.') {
                            module_name.split('.').next_back().unwrap().to_string()
                        } else {
                            module_name.to_string()
                        }
                    },
                    std::string::ToString::to_string,
                );

                self.imported_symbols
                    .entry(namespace.to_string())
                    .or_insert(module_symbols.clone());
                self.add_import_symbol_if_absent(
                    &namespace,
                    Symbol {
                        kind: SymbolKind::Import,
                        span,
                        type_: Some(Type::Module(namespace.to_string())),
                        interfaces: std::collections::HashMap::new(),
                        methods: std::collections::HashMap::new(),
                        fields: std::collections::HashMap::new(),
                        type_params: Vec::new(),
                        original_name: None,
                        llvm_name: None,
                        default_param_count: 0,
                        variants: None,
                    },
                )?;
            }
            ImportSpec::Item { item, alias } => {
                let symbol_name = alias.as_ref().unwrap_or(item);
                if let Some(symbol) = module_symbols.get(item) {
                    self.add_import_symbol_if_absent(symbol_name, symbol.clone())?;
                }
            }
            ImportSpec::Wildcard => {
                for (name, symbol) in module_symbols {
                    self.add_import_symbol_if_absent(&name, symbol)?;
                }
            }
            ImportSpec::Items { items } => {
                for (item, alias) in items {
                    let symbol_name = alias.as_ref().unwrap_or(item);
                    if let Some(symbol) = module_symbols.get(item) {
                        self.add_import_symbol_if_absent(symbol_name, symbol.clone())?;
                    }
                }
            }
        }
        Ok(())
    }
}
