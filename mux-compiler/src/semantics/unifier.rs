use crate::diagnostic::DiagnosticCode;
use crate::lexer::Span;
use crate::semantics::error::SemanticError;
use crate::semantics::format::format_type;
use crate::semantics::types::Type;
use std::collections::HashMap;

#[derive(Debug, Clone, Default)]
pub struct Unifier {
    pub substitutions: HashMap<String, Type>,
}

impl Unifier {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // unifies two types, returning an error with the provided span on mismatch.
    pub fn unify(&mut self, a: &Type, b: &Type, span: Span) -> Result<(), SemanticError> {
        match (a, b) {
            (Type::Variable(var), t) | (t, Type::Variable(var)) => {
                // If t is the same type variable, they're compatible
                if let Type::Variable(tvar) = t
                    && tvar == var
                {
                    return Ok(());
                }
                if let Some(existing) = self.substitutions.get(var).cloned() {
                    self.unify(&existing, t, span)?;
                } else {
                    // occurs check, ensure var not in t (but not if t is the same variable)
                    if self.occurs(var, t) {
                        return Err(SemanticError {
                            code: DiagnosticCode::TypeMismatch,
                            message: format!(
                                "Recursive type: {} occurs in {}",
                                var,
                                format_type(t)
                            )
                            .into_boxed_str(),
                            help: None,
                            span,
                            file_id: None,
                            span_edits: None,
                        });
                    }
                    self.substitutions.insert(var.clone(), t.clone());
                }
            }
            (Type::Primitive(p1), Type::Primitive(p2)) if p1 == p2 => {}
            (Type::Named(n1, args1), Type::Named(n2, args2))
                if n1 == n2 && args1.len() == args2.len() =>
            {
                for (a1, a2) in args1.iter().zip(args2) {
                    self.unify(a1, a2, span)?;
                }
            }
            (
                Type::Function {
                    params: p1,
                    returns: r1,
                    ..
                },
                Type::Function {
                    params: p2,
                    returns: r2,
                    ..
                },
            ) if p1.len() == p2.len() => {
                for (a1, a2) in p1.iter().zip(p2) {
                    self.unify(a1, a2, span)?;
                }
                self.unify(r1, r2, span)?;
            }
            (Type::Reference(t1), Type::Reference(t2)) => self.unify(t1, t2, span)?,
            (Type::List(t1), Type::List(t2)) => self.unify(t1, t2, span)?,
            (Type::Map(k1, v1), Type::Map(k2, v2)) => {
                self.unify(k1, k2, span)?;
                self.unify(v1, v2, span)?;
            }
            (Type::Set(t1), Type::Set(t2)) => self.unify(t1, t2, span)?,
            (Type::Tuple(l1, r1), Type::Tuple(l2, r2)) => {
                self.unify(l1, l2, span)?;
                self.unify(r1, r2, span)?;
            }

            (Type::Optional(t1), Type::Optional(t2)) => self.unify(t1, t2, span)?,
            (Type::Result(o1, e1), Type::Result(o2, e2)) => {
                self.unify(o1, o2, span)?;
                self.unify(e1, e2, span)?;
            }
            (Type::Void, Type::Void) => {}
            (Type::EmptyList, Type::EmptyList) => {}
            (Type::EmptyMap, Type::EmptyMap) => {}
            (Type::EmptySet, Type::EmptySet) => {}
            (Type::List(_), Type::EmptyList) | (Type::EmptyList, Type::List(_)) => {}
            (Type::Map(_, _), Type::EmptyMap) | (Type::EmptyMap, Type::Map(_, _)) => {}
            (Type::Set(_), Type::EmptySet) | (Type::EmptySet, Type::Set(_)) => {}
            (Type::Never, _) => {}
            (_, Type::Never) => {}
            _ => {
                return Err(SemanticError::new(
                    DiagnosticCode::TypeMismatch,
                    format!(
                        "Type mismatch: expected {}, got {}",
                        format_type(a),
                        format_type(b)
                    ),
                    span,
                ));
            }
        }
        Ok(())
    }

    #[allow(clippy::only_used_in_recursion)]
    fn occurs(&self, var: &str, t: &Type) -> bool {
        match t {
            Type::Variable(v) if v == var => true,
            Type::Named(_, args) => args.iter().any(|arg| self.occurs(var, arg)),
            Type::Function {
                params, returns, ..
            } => params.iter().any(|p| self.occurs(var, p)) || self.occurs(var, returns),
            Type::Reference(inner)
            | Type::List(inner)
            | Type::Set(inner)
            | Type::Optional(inner) => self.occurs(var, inner),
            Type::Result(ok, err) => self.occurs(var, ok) || self.occurs(var, err),
            Type::Map(k, v) => self.occurs(var, k) || self.occurs(var, v),
            Type::Tuple(l, r) => self.occurs(var, l) || self.occurs(var, r),

            _ => false,
        }
    }

    #[must_use]
    pub fn apply(&self, t: &Type) -> Type {
        match t {
            Type::Variable(var) => self
                .substitutions
                .get(var)
                .cloned()
                .unwrap_or_else(|| t.clone()),
            Type::Named(name, args) => {
                let applied_args: Vec<Type> = args.iter().map(|arg| self.apply(arg)).collect();
                Type::Named(name.clone(), applied_args)
            }
            Type::Function {
                params,
                returns,
                default_count,
            } => Type::Function {
                params: params.iter().map(|p| self.apply(p)).collect(),
                returns: Box::new(self.apply(returns)),
                default_count: *default_count,
            },
            Type::Reference(inner) => Type::Reference(Box::new(self.apply(inner))),
            Type::List(inner) => Type::List(Box::new(self.apply(inner))),
            Type::Map(k, v) => Type::Map(Box::new(self.apply(k)), Box::new(self.apply(v))),
            Type::Set(inner) => Type::Set(Box::new(self.apply(inner))),
            Type::Tuple(l, r) => Type::Tuple(Box::new(self.apply(l)), Box::new(self.apply(r))),
            Type::Optional(inner) => Type::Optional(Box::new(self.apply(inner))),
            Type::Result(ok, err) => {
                Type::Result(Box::new(self.apply(ok)), Box::new(self.apply(err)))
            }
            _ => t.clone(),
        }
    }
}
