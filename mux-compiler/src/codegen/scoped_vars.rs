//! The code generator's variable table, scoped by block.
//!
//! This was a flat `HashMap` for the whole function, which made a binding
//! outlive the block that introduced it. Two things went wrong with that. A name
//! bound inside a block stayed visible afterwards, so `auto x = 42` followed by
//! `for int x in nums` left `x` naming the loop variable for the rest of the
//! function. And because `declare_variable` reuses the slot of a live binding of
//! the same name - which is how a loop-local reuses its storage each iteration
//! instead of leaking one allocation per pass - a block that shadowed an outer
//! name wrote *through* to the outer variable's storage.
//!
//! Both follow from one missing distinction: whether a name is already bound in
//! the block being compiled, or merely somewhere outside it. A stack of scopes
//! answers that, so a redeclaration in the same scope reuses its slot and a
//! shadowing declaration in an inner scope gets its own.

use std::collections::HashMap;

/// A variable table whose bindings are scoped to the block that introduced
/// them.
///
/// The read API deliberately mirrors `HashMap`: `get` walks outward from the
/// innermost scope, so lookups read as they did before this was scoped, and
/// `insert` binds in the innermost scope, which is where a declaration belongs.
#[derive(Debug, Clone)]
pub(super) struct ScopedVars<V> {
    /// Innermost scope last. Never empty: the function scope is the first
    /// entry, and `pop_scope` will not remove it.
    scopes: Vec<HashMap<String, V>>,
}

impl<V> Default for ScopedVars<V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<V> ScopedVars<V> {
    pub(super) fn new() -> Self {
        Self {
            scopes: vec![HashMap::new()],
        }
    }

    /// Start from an existing set of bindings, all in the function scope.
    ///
    /// Module init begins with the module's globals already bound, so a
    /// top-level declaration reuses its pre-declared global slot rather than
    /// allocating a second one.
    pub(super) fn from_bindings(bindings: HashMap<String, V>) -> Self {
        Self {
            scopes: vec![bindings],
        }
    }

    /// Open a block scope. Bindings made until the matching `pop_scope` are
    /// dropped with it.
    pub(super) fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    /// Close the innermost block scope, discarding its bindings.
    ///
    /// The function scope is kept: unbalanced calls should not leave the table
    /// with nowhere to bind, and codegen errors are terminal anyway.
    pub(super) fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// The innermost binding of `name`, searching outward.
    pub(super) fn get(&self, name: &str) -> Option<&V> {
        self.scopes.iter().rev().find_map(|scope| scope.get(name))
    }

    /// Bind `name` in the innermost scope, shadowing any outer binding.
    pub(super) fn insert(&mut self, name: String, value: V) {
        self.innermost().insert(name, value);
    }

    /// The binding of `name` in the innermost scope only.
    ///
    /// This is the question `declare_variable` has to ask: a redeclaration in
    /// the same scope - a loop-local on its second iteration - reuses its slot,
    /// while a declaration that merely shares a name with something further out
    /// is a new variable and needs storage of its own.
    pub(super) fn get_in_current_scope(&self, name: &str) -> Option<&V> {
        self.scopes
            .last()
            .expect("the function scope is never popped")
            .get(name)
    }

    /// Every visible binding, innermost first, with shadowed outer bindings
    /// omitted so each name appears once.
    pub(super) fn iter(&self) -> impl Iterator<Item = (&String, &V)> {
        let mut seen = std::collections::HashSet::new();
        self.scopes
            .iter()
            .rev()
            .flat_map(std::collections::HashMap::iter)
            .filter(move |(name, _)| seen.insert((*name).clone()))
    }

    /// Drop every binding and every block scope, leaving one empty function
    /// scope. Used when generation moves to another function.
    pub(super) fn clear(&mut self) {
        self.scopes.clear();
        self.scopes.push(HashMap::new());
    }

    fn innermost(&mut self) -> &mut HashMap<String, V> {
        self.scopes
            .last_mut()
            .expect("the function scope is never popped")
    }
}
