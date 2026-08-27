use crate::diagnostic::DiagnosticCode;
use crate::lexer::Span;
use crate::semantics::error::SemanticError;
use crate::semantics::types::{Symbol, SymbolKind};
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

// The canonical stdlib items live in `crate::semantics::stdlib`.

#[derive(Debug)]
pub struct SymbolTable {
    scopes: Vec<Rc<RefCell<Scope>>>,
    pub all_symbols: HashMap<String, Symbol>,
    /// How many scopes code generation has pushed on top of the analysis
    /// scopes. Only those may be written to or popped by the codegen helpers
    /// below, so an unbalanced call can never clobber or discard a scope that
    /// analysis owns.
    codegen_scope_depth: usize,
}

#[derive(Debug, Default)]
struct Scope {
    symbols: HashMap<String, Symbol>,
    children: Vec<Rc<RefCell<Scope>>>,
}

impl Default for SymbolTable {
    fn default() -> Self {
        Self::new()
    }
}

impl SymbolTable {
    pub fn new() -> Self {
        let root = Rc::new(RefCell::new(Scope::default()));
        SymbolTable {
            scopes: vec![root],
            all_symbols: HashMap::new(),
            codegen_scope_depth: 0,
        }
    }

    /// Open a scope for the locals of the function code generation is emitting.
    ///
    /// Analysis pops every scope before code generation starts, so a name would
    /// otherwise resolve through `all_symbols`, a flat program-wide index where
    /// the last function to declare a name answers for all of them - two
    /// functions with a same-named parameter of different types each got the
    /// other's. Code generation knows exactly what is in scope in the function
    /// it is emitting, so it supplies that here and the analyzer resolves
    /// against it.
    ///
    /// Nested generation (a specialized method emitted mid-body) opens its own
    /// scope and closes it again, so the inner function shadows the outer one
    /// for as long as it is being emitted, which is what the scope chain
    /// already means.
    pub fn open_codegen_scope(&mut self) {
        self.scopes.push(Rc::new(RefCell::new(Scope::default())));
        self.codegen_scope_depth += 1;
    }

    /// Close the innermost scope opened by `open_codegen_scope`.
    pub fn close_codegen_scope(&mut self) {
        if self.codegen_scope_depth == 0 {
            return;
        }
        self.scopes.pop();
        self.codegen_scope_depth -= 1;
    }

    /// Replace the contents of the innermost code generation scope.
    ///
    /// Inserted raw rather than through `add_symbol`: these names were already
    /// checked when the function was analyzed, and re-running the duplicate and
    /// type-collision rules over them would reject rebindings that analysis
    /// allowed.
    pub fn set_codegen_locals<I>(&mut self, locals: I)
    where
        I: IntoIterator<Item = (String, Symbol)>,
    {
        if self.codegen_scope_depth == 0 {
            return;
        }
        let Some(scope) = self.scopes.last() else {
            return;
        };
        let mut scope = scope.borrow_mut();
        scope.symbols.clear();
        for (name, symbol) in locals {
            scope.symbols.insert(name, symbol);
        }
    }

    pub fn push_scope(&mut self) -> Result<(), SemanticError> {
        let new_scope = Rc::new(RefCell::new(Scope::default()));
        self.scopes
            .last()
            .expect("at least global scope should exist")
            .borrow_mut()
            .children
            .push(Rc::clone(&new_scope));
        self.scopes.push(new_scope);
        Ok(())
    }

    pub fn pop_scope(&mut self) -> Result<(), SemanticError> {
        if self.scopes.len() <= 1 {
            return Err(SemanticError {
                code: DiagnosticCode::InternalCompiler,
                message: "Cannot pop the global scope".into(),
                help: None,
                span: Span::new(0, 0), // Internal error, no user span available
                file_id: None,
                span_edits: None,
            });
        }
        self.scopes.pop();
        Ok(())
    }

    pub fn exists(&self, name: &str) -> bool {
        self.get_cloned(name).is_some()
    }

    fn add_symbol_to_current_scope(
        &mut self,
        name: &str,
        symbol: Symbol,
    ) -> Result<(), SemanticError> {
        if self.scopes.is_empty() {
            return Err(SemanticError {
                code: DiagnosticCode::InternalCompiler,
                message: "No active scope".into(),
                help: None,
                span: Span::new(0, 0), // Internal error, no user span available
                file_id: None,
                span_edits: None,
            });
        }

        // A bare '_' is a hole, not a name: it marks a parameter or binding as
        // deliberately unused. Nothing can refer to it - '_' is not an
        // expression - so it never needs to be in scope, and entering it made
        // a second '_' in the same signature a "Duplicate declaration of '_'".
        // Names that merely START with an underscore are ordinary identifiers
        // and bind as usual.
        if name == "_" {
            return Ok(());
        }

        if let Some(err) = self.reject_type_name_collision(name, &symbol) {
            return Err(err);
        }

        let current = self
            .scopes
            .last()
            .expect("at least global scope should exist");
        let mut current_borrow = current.borrow_mut();

        if current_borrow.symbols.contains_key(name) {
            // A built-in occupies the global scope, so a module-level
            // declaration taking its name reads as a duplicate of something the
            // author never wrote. Name it instead - but only at that scope: in
            // any inner scope the clash really is with another declaration the
            // author can see, and calling it a built-in would send them looking
            // for the wrong thing.
            let is_global_scope = self.scopes.len() == 1;
            if is_global_scope && crate::semantics::stdlib::BUILT_IN_FUNCTIONS.contains_key(name) {
                return Err(SemanticError::with_help(
                    DiagnosticCode::ImportFailure,
                    format!("'{}' is a built-in function", name),
                    symbol.span,
                    format!("Rename this; '{}' is taken at module scope.", name),
                ));
            }
            return Err(SemanticError {
                code: DiagnosticCode::DuplicateDeclaration,
                message: format!("Duplicate declaration of '{}'", name).into_boxed_str(),
                help: None,
                span: symbol.span,
                file_id: None,
                span_edits: None,
            });
        }

        current_borrow
            .symbols
            .insert(name.to_string(), symbol.clone());
        if self.should_track_global_symbol(name, &symbol) {
            self.all_symbols.insert(name.to_string(), symbol);
        }
        Ok(())
    }

    /// Reject a variable whose name is already a declared class, enum or
    /// interface.
    ///
    /// `all_symbols` is a flat, program-wide, last-write-wins index, and
    /// `lookup` reads only from it - the scope chain beside it is not consulted.
    /// So a local named `Color` did not shadow the enum `Color`, it replaced it
    /// everywhere, and the enum then dropped out of codegen's registry and
    /// failed from functions that never mentioned the local (issue #367).
    ///
    /// Rejecting the collision removes that silently destructive case without
    /// auditing the 45 call sites that read the flat index. It is a mitigation,
    /// not the full fix: shadowing between two values is still unchecked, and
    /// `lookup` is still scope-blind. If types and values are given separate
    /// namespaces later, this simply stops firing.
    fn reject_type_name_collision(&self, name: &str, symbol: &Symbol) -> Option<SemanticError> {
        if symbol.kind != SymbolKind::Variable {
            return None;
        }
        let existing = self.all_symbols.get(name)?;
        let kind = match existing.kind {
            SymbolKind::Class => "a class",
            SymbolKind::Enum => "an enum",
            SymbolKind::Interface => "an interface",
            _ => return None,
        };
        Some(SemanticError::with_help(
            DiagnosticCode::InvalidOperation,
            format!("Cannot declare a variable named '{}'", name),
            symbol.span,
            format!(
                "'{}' already names {}. Rename the variable; a type name and a value cannot share a name.",
                name, kind
            ),
        ))
    }

    pub fn add_symbol(&mut self, name: &str, symbol: Symbol) -> Result<(), SemanticError> {
        self.add_symbol_to_current_scope(name, symbol)
    }

    fn should_track_global_symbol(&self, name: &str, symbol: &Symbol) -> bool {
        let _ = symbol;
        name != "self"
    }

    pub fn add_imported_symbol(&mut self, name: &str, symbol: Symbol) -> Result<(), SemanticError> {
        self.add_symbol_to_current_scope(name, symbol)
    }

    pub fn global_scope_symbols(&self) -> HashMap<String, Symbol> {
        self.scopes
            .first()
            .map(|scope| scope.borrow().symbols.clone())
            .unwrap_or_default()
    }

    /// Resolve `name`, preferring whatever is actually in scope.
    ///
    /// `all_symbols` is a flat, program-wide, last-write-wins index. Reading it
    /// first meant the last function to declare a name won everywhere, so two
    /// functions with a parameter of the same name and different types resolved
    /// to each other's. The scope chain answers correctly whenever the name is
    /// live; the flat index remains the fallback for everything looked up after
    /// its scope was popped, which is how codegen reads declarations.
    pub fn lookup(&self, name: &str) -> Option<Symbol> {
        self.get_cloned(name)
            .or_else(|| self.all_symbols.get(name).cloned())
    }

    pub fn get_cloned(&self, name: &str) -> Option<Symbol> {
        for scope in self.scopes.iter().rev() {
            let scope_borrow = scope.borrow();
            if let Some(symbol) = scope_borrow.symbols.get(name) {
                return Some(symbol.clone());
            }
        }
        None
    }

    /// Find symbols with names similar to the given name (for "did you mean?" suggestions).
    /// Uses a simple edit distance check to find candidates within a threshold.
    pub fn find_similar(&self, name: &str) -> Option<String> {
        let threshold = calculate_similarity_threshold(name);

        let mut best: Option<(String, usize, u8)> = None;

        // Check all scopes
        for scope in self.scopes.iter().rev() {
            let scope_borrow = scope.borrow();
            best = Self::find_best_match(name, threshold, 0, scope_borrow.symbols.keys(), best);
        }

        // Also check all_symbols (hoisted functions, classes, etc.)
        best = Self::find_best_match(name, threshold, 1, self.all_symbols.keys(), best);

        // Check built-in functions
        best = Self::find_best_match(
            name,
            threshold,
            2,
            crate::semantics::stdlib::BUILT_IN_FUNCTIONS.keys().copied(),
            best,
        );

        best.map(|(name, _, _)| name)
    }

    fn find_best_match<S: AsRef<str>>(
        name: &str,
        threshold: usize,
        source_rank: u8,
        candidates: impl Iterator<Item = S>,
        best: Option<(String, usize, u8)>,
    ) -> Option<(String, usize, u8)> {
        let mut current_best = best;
        for candidate in candidates {
            let s = candidate.as_ref();
            let dist = edit_distance(name, s);
            if dist > threshold {
                continue;
            }

            if current_best
                .as_ref()
                .is_none_or(|(best_name, best_dist, best_rank)| {
                    dist < *best_dist
                        || (dist == *best_dist
                            && (source_rank < *best_rank
                                || (source_rank == *best_rank && s < best_name.as_str())))
                })
            {
                current_best = Some((s.to_string(), dist, source_rank));
            }
        }
        current_best
    }
}

/// Calculate the maximum allowed edit distance for suggesting similar names.
/// Uses an adaptive threshold based on name length:
/// - 1-2 chars: threshold of 1 (strict for short names)
/// - 3-5 chars: threshold of 2 (moderate for medium names)
/// - 6+ chars: threshold of 3 (permissive for long names)
pub fn calculate_similarity_threshold(name: &str) -> usize {
    match name.len() {
        0..=2 => 1,
        3..=5 => 2,
        _ => 3,
    }
}

/// Compute the Levenshtein edit distance between two strings.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a_len = a.chars().count();
    let b_len = b.chars().count();

    if a_len == 0 {
        return b_len;
    }
    if b_len == 0 {
        return a_len;
    }

    let mut prev: Vec<usize> = (0..=b_len).collect();
    let mut curr = vec![0; b_len + 1];

    for (i, ca) in a.chars().enumerate() {
        curr[0] = i + 1;
        for (j, cb) in b.chars().enumerate() {
            let cost = if ca == cb { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[b_len]
}
