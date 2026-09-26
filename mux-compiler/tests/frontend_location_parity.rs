use mux_lang::ast::{
    AstNode, ExpressionKind, ExpressionNode, FunctionNode, StatementKind, StatementNode, TypeKind,
    TypeNode,
};
use mux_lang::lexer::{Lexer, Span};
use mux_lang::parser::Parser;
use mux_lang::source::Source;
use mux_lang::syntax;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn fixtures() -> Vec<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../test_scripts");
    let mut paths: Vec<_> = fs::read_dir(root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "mux"))
        .collect();
    paths.sort();
    paths
}

fn record(spans: &mut BTreeMap<String, Span>, path: String, span: Span) {
    assert!(
        spans.insert(path.clone(), span).is_none(),
        "duplicate span path {path}"
    );
}

fn indexed(path: &str, index: usize) -> String {
    format!("{path}[{index}]")
}

fn collect_type(node: &TypeNode, path: &str, spans: &mut BTreeMap<String, Span>) {
    record(spans, path.to_owned(), node.span);
    match &node.kind {
        TypeKind::Primitive(_) | TypeKind::Auto => {}
        TypeKind::Named(_, args) => {
            for (index, arg) in args.iter().enumerate() {
                collect_type(arg, &indexed(&format!("{path}.type_args"), index), spans);
            }
        }
        TypeKind::TraitObject(inner)
        | TypeKind::Reference(inner)
        | TypeKind::List(inner)
        | TypeKind::Set(inner) => collect_type(inner, &format!("{path}.inner"), spans),
        TypeKind::Function { params, returns } => {
            for (index, param) in params.iter().enumerate() {
                collect_type(param, &indexed(&format!("{path}.params"), index), spans);
            }
            collect_type(returns, &format!("{path}.returns"), spans);
        }
        TypeKind::Map(key, value) | TypeKind::Tuple(key, value) => {
            collect_type(key, &format!("{path}.first"), spans);
            collect_type(value, &format!("{path}.second"), spans);
        }
    }
}

fn collect_expression(node: &ExpressionNode, path: &str, spans: &mut BTreeMap<String, Span>) {
    record(spans, format!("{path}.span"), node.span);
    match &node.kind {
        ExpressionKind::Literal(_) | ExpressionKind::None | ExpressionKind::Identifier(_) => {}
        ExpressionKind::Binary {
            left,
            op_span,
            right,
            ..
        } => {
            record(spans, format!("{path}.op_span"), *op_span);
            collect_expression(left, &format!("{path}.left"), spans);
            collect_expression(right, &format!("{path}.right"), spans);
        }
        ExpressionKind::Unary { op_span, expr, .. } => {
            record(spans, format!("{path}.op_span"), *op_span);
            collect_expression(expr, &format!("{path}.expr"), spans);
        }
        ExpressionKind::Call { func, args } => {
            collect_expression(func, &format!("{path}.func"), spans);
            for (index, arg) in args.iter().enumerate() {
                collect_expression(arg, &indexed(&format!("{path}.args"), index), spans);
            }
        }
        ExpressionKind::FieldAccess { expr, .. } => {
            collect_expression(expr, &format!("{path}.expr"), spans);
        }
        ExpressionKind::ListAccess { expr, index } => {
            collect_expression(expr, &format!("{path}.expr"), spans);
            collect_expression(index, &format!("{path}.index"), spans);
        }
        ExpressionKind::Slice { expr, start, end } => {
            collect_expression(expr, &format!("{path}.expr"), spans);
            if let Some(start) = start {
                collect_expression(start, &format!("{path}.start"), spans);
            }
            if let Some(end) = end {
                collect_expression(end, &format!("{path}.end"), spans);
            }
        }
        ExpressionKind::ListLiteral(items)
        | ExpressionKind::SetLiteral(items)
        | ExpressionKind::TupleLiteral(items) => {
            for (index, item) in items.iter().enumerate() {
                collect_expression(item, &indexed(&format!("{path}.items"), index), spans);
            }
        }
        ExpressionKind::MapLiteral {
            key_type,
            value_type,
            entries,
        } => {
            collect_type(key_type, &format!("{path}.key_type"), spans);
            collect_type(value_type, &format!("{path}.value_type"), spans);
            for (index, (key, value)) in entries.iter().enumerate() {
                let entry_path = indexed(&format!("{path}.entries"), index);
                collect_expression(key, &format!("{entry_path}.key"), spans);
                collect_expression(value, &format!("{entry_path}.value"), spans);
            }
        }
        ExpressionKind::If {
            cond,
            then_expr,
            else_expr,
        } => {
            collect_expression(cond, &format!("{path}.cond"), spans);
            collect_expression(then_expr, &format!("{path}.then"), spans);
            collect_expression(else_expr, &format!("{path}.else"), spans);
        }
        ExpressionKind::Match { expr, arms } => {
            collect_expression(expr, &format!("{path}.expr"), spans);
            for (index, arm) in arms.iter().enumerate() {
                collect_arm(arm, &indexed(&format!("{path}.arms"), index), spans);
            }
        }
        ExpressionKind::Lambda {
            params,
            return_type,
            body,
            where_clause,
        } => {
            for (index, param) in params.iter().enumerate() {
                let param_path = indexed(&format!("{path}.params"), index);
                collect_type(&param.type_, &format!("{param_path}.type"), spans);
                if let Some(default) = &param.default_value {
                    collect_expression(default, &format!("{param_path}.default"), spans);
                }
            }
            collect_type(return_type, &format!("{path}.return_type"), spans);
            for (index, stmt) in body.iter().enumerate() {
                collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
            }
            if let Some(clause) = where_clause {
                collect_where_clause(clause, &format!("{path}.where_clause"), spans);
            }
        }
        ExpressionKind::GenericType(_, args) => {
            for (index, arg) in args.iter().enumerate() {
                collect_type(arg, &indexed(&format!("{path}.type_args"), index), spans);
            }
        }
    }
}

fn collect_where_clause(
    clause: &mux_lang::ast::WhereClause,
    path: &str,
    spans: &mut BTreeMap<String, Span>,
) {
    record(spans, format!("{path}.span"), clause.span);
    for (index, predicate) in clause.predicates.iter().enumerate() {
        collect_expression(
            predicate,
            &indexed(&format!("{path}.predicates"), index),
            spans,
        );
    }
}

fn collect_bounds(
    bounds: &[mux_lang::ast::TraitBound],
    path: &str,
    spans: &mut BTreeMap<String, Span>,
) {
    for (index, bound) in bounds.iter().enumerate() {
        let bound_path = indexed(path, index);
        record(spans, format!("{bound_path}.span"), bound.span);
        for (type_index, ty) in bound.type_params.iter().enumerate() {
            collect_type(
                ty,
                &indexed(&format!("{bound_path}.type_params"), type_index),
                spans,
            );
        }
    }
}

fn collect_type_params(
    type_params: &[(String, Vec<mux_lang::ast::TraitBound>)],
    path: &str,
    spans: &mut BTreeMap<String, Span>,
) {
    for (index, (_, bounds)) in type_params.iter().enumerate() {
        collect_bounds(
            bounds,
            &indexed(&format!("{path}.type_params"), index),
            spans,
        );
    }
}

fn collect_function(node: &FunctionNode, path: &str, spans: &mut BTreeMap<String, Span>) {
    record(spans, format!("{path}.span"), node.span);
    collect_type_params(&node.type_params, path, spans);
    for (index, param) in node.params.iter().enumerate() {
        let param_path = indexed(&format!("{path}.params"), index);
        collect_type(&param.type_, &format!("{param_path}.type"), spans);
        if let Some(default) = &param.default_value {
            collect_expression(default, &format!("{param_path}.default"), spans);
        }
    }
    collect_type(&node.return_type, &format!("{path}.return_type"), spans);
    for (index, stmt) in node.body.iter().enumerate() {
        collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
    }
    if let Some(clause) = &node.where_clause {
        collect_where_clause(clause, &format!("{path}.where_clause"), spans);
    }
}

fn collect_arm(arm: &mux_lang::ast::MatchArm, path: &str, spans: &mut BTreeMap<String, Span>) {
    if let Some(guard) = &arm.guard {
        collect_expression(guard, &format!("{path}.guard"), spans);
    }
    for (index, stmt) in arm.body.iter().enumerate() {
        collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
    }
}

fn collect_statement(node: &StatementNode, path: &str, spans: &mut BTreeMap<String, Span>) {
    record(spans, format!("{path}.span"), node.span);
    match &node.kind {
        StatementKind::AutoDecl(_, ty, expr)
        | StatementKind::TypedDecl(_, ty, expr)
        | StatementKind::ConstDecl(_, ty, expr) => {
            collect_type(ty, &format!("{path}.type"), spans);
            collect_expression(expr, &format!("{path}.value"), spans);
        }
        StatementKind::UninitDecl(_, ty) => collect_type(ty, &format!("{path}.type"), spans),
        StatementKind::Function(func) => collect_function(func, &format!("{path}.function"), spans),
        StatementKind::Import { .. } | StatementKind::Break | StatementKind::Continue => {}
        StatementKind::Return(expr) => {
            if let Some(expr) = expr {
                collect_expression(expr, &format!("{path}.value"), spans);
            }
        }
        StatementKind::If {
            cond,
            then_block,
            else_block,
        } => {
            collect_expression(cond, &format!("{path}.cond"), spans);
            for (index, stmt) in then_block.iter().enumerate() {
                collect_statement(stmt, &indexed(&format!("{path}.then"), index), spans);
            }
            if let Some(block) = else_block {
                for (index, stmt) in block.iter().enumerate() {
                    collect_statement(stmt, &indexed(&format!("{path}.else"), index), spans);
                }
            }
        }
        StatementKind::While { cond, body } => {
            collect_expression(cond, &format!("{path}.cond"), spans);
            for (index, stmt) in body.iter().enumerate() {
                collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
            }
        }
        StatementKind::For {
            var_type,
            iter,
            body,
            ..
        } => {
            collect_type(var_type, &format!("{path}.var_type"), spans);
            collect_expression(iter, &format!("{path}.iter"), spans);
            for (index, stmt) in body.iter().enumerate() {
                collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
            }
        }
        StatementKind::Match { expr, arms } => {
            collect_expression(expr, &format!("{path}.expr"), spans);
            for (index, arm) in arms.iter().enumerate() {
                collect_arm(arm, &indexed(&format!("{path}.arms"), index), spans);
            }
        }
        StatementKind::Expression(expr) => collect_expression(expr, &format!("{path}.expr"), spans),
        StatementKind::Block(body) => {
            for (index, stmt) in body.iter().enumerate() {
                collect_statement(stmt, &indexed(&format!("{path}.body"), index), spans);
            }
        }
    }
}

fn collect_ast(nodes: &[AstNode]) -> BTreeMap<String, Span> {
    let mut spans = BTreeMap::new();
    for (index, node) in nodes.iter().enumerate() {
        let path = indexed("ast", index);
        match node {
            AstNode::Function(function) => {
                collect_function(function, &format!("{path}.function"), &mut spans)
            }
            AstNode::Statement(statement) => {
                collect_statement(statement, &format!("{path}.statement"), &mut spans)
            }
            AstNode::Test { body, span, .. } => {
                record(&mut spans, format!("{path}.span"), *span);
                for (body_index, stmt) in body.iter().enumerate() {
                    collect_statement(
                        stmt,
                        &indexed(&format!("{path}.body"), body_index),
                        &mut spans,
                    );
                }
            }
            AstNode::Class {
                type_params,
                traits,
                fields,
                methods,
                where_clause,
                span,
                ..
            } => {
                record(&mut spans, format!("{path}.span"), *span);
                collect_type_params(type_params, &path, &mut spans);
                for (trait_index, trait_ref) in traits.iter().enumerate() {
                    let trait_path = indexed(&format!("{path}.traits"), trait_index);
                    record(&mut spans, format!("{trait_path}.span"), trait_ref.span);
                    for (arg_index, ty) in trait_ref.type_args.iter().enumerate() {
                        collect_type(
                            ty,
                            &indexed(&format!("{trait_path}.type_args"), arg_index),
                            &mut spans,
                        );
                    }
                }
                collect_fields(fields, &format!("{path}.fields"), &mut spans);
                for (method_index, method) in methods.iter().enumerate() {
                    collect_function(
                        method,
                        &indexed(&format!("{path}.methods"), method_index),
                        &mut spans,
                    );
                }
                if let Some(clause) = where_clause {
                    collect_where_clause(clause, &format!("{path}.where_clause"), &mut spans);
                }
            }
            AstNode::Interface {
                type_params,
                fields,
                methods,
                span,
                ..
            } => {
                record(&mut spans, format!("{path}.span"), *span);
                collect_type_params(type_params, &path, &mut spans);
                collect_fields(fields, &format!("{path}.fields"), &mut spans);
                for (method_index, method) in methods.iter().enumerate() {
                    collect_function(
                        method,
                        &indexed(&format!("{path}.methods"), method_index),
                        &mut spans,
                    );
                }
            }
            AstNode::Enum {
                type_params,
                variants,
                span,
                ..
            } => {
                record(&mut spans, format!("{path}.span"), *span);
                collect_type_params(type_params, &path, &mut spans);
                for (variant_index, variant) in variants.iter().enumerate() {
                    if let Some(data) = &variant.data {
                        for (field_index, (_, ty)) in data.iter().enumerate() {
                            collect_type(
                                ty,
                                &format!(
                                    "{path}.variants[{variant_index}].fields[{field_index}].type"
                                ),
                                &mut spans,
                            );
                        }
                    }
                    if let Some(clause) = &variant.where_clause {
                        collect_where_clause(
                            clause,
                            &format!("{path}.variants[{variant_index}].where_clause"),
                            &mut spans,
                        );
                    }
                }
            }
        }
    }
    spans
}

fn collect_fields(fields: &[mux_lang::ast::Field], path: &str, spans: &mut BTreeMap<String, Span>) {
    for (index, field) in fields.iter().enumerate() {
        let field_path = indexed(path, index);
        collect_type(&field.type_, &format!("{field_path}.type"), spans);
        if let Some(default) = &field.default_value {
            collect_expression(default, &format!("{field_path}.default"), spans);
        }
        if let Some(clause) = &field.where_clause {
            collect_where_clause(clause, &format!("{field_path}.where_clause"), spans);
        }
    }
}

fn location_mismatches(
    fixture: &Path,
    path: &str,
    left: Span,
    right: Span,
    source: &Source,
) -> Vec<String> {
    let mut mismatches = Vec::new();
    for (field, legacy, syntax) in [
        (
            "row_start",
            format!("{}", left.row_start),
            format!("{}", right.row_start),
        ),
        (
            "byte_range",
            format!("{:?}", left.byte_range),
            format!("{:?}", right.byte_range),
        ),
    ] {
        if legacy != syntax {
            mismatches.push(format!(
                "{}: {path}.{field}: legacy {legacy}, syntax {syntax}",
                fixture.display()
            ));
        }
    }
    if left.col_start != right.col_start {
        let syntax_start_is_authoritative = right.byte_range.is_some_and(|range| {
            Some(range) == left.byte_range
                && source.line_col(range.start) == (right.row_start, right.col_start)
        });
        if !syntax_start_is_authoritative {
            mismatches.push(format!(
                "{}: {path}.col_start: legacy {}, syntax {}",
                fixture.display(),
                left.col_start,
                right.col_start
            ));
        }
    }
    mismatches
}

fn range_location_mismatches(
    fixture: &Path,
    path: &str,
    span: Span,
    source: &Source,
) -> Vec<String> {
    let Some(range) = span.byte_range else {
        return vec![format!(
            "{}: {path}.byte_range: syntax span has no authoritative byte range",
            fixture.display()
        )];
    };
    let (row_start, col_start) = source.line_col(range.start);
    let (row_end, col_end) = source.line_col(range.end);
    let mut mismatches = Vec::new();
    for (field, actual, expected) in [
        ("row_start", span.row_start, row_start),
        ("col_start", span.col_start, col_start),
        ("row_end", span.row_end.unwrap_or_default(), row_end),
        ("col_end", span.col_end.unwrap_or_default(), col_end),
    ] {
        if actual != expected
            || ((field == "row_end" || field == "col_end")
                && (span.row_end.is_none() || span.col_end.is_none()))
        {
            mismatches.push(format!(
                "{}: syntax {path}.{field} {:?} disagrees with byte_range {:?} (expected {expected})",
                fixture.display(),
                match field {
                    "row_start" => format!("{}", span.row_start),
                    "col_start" => format!("{}", span.col_start),
                    "row_end" => format!("{:?}", span.row_end),
                    _ => format!("{:?}", span.col_end),
                },
                range
            ));
        }
    }
    mismatches
}

#[test]
fn legacy_and_syntax_frontends_agree_on_all_fixture_locations() {
    let mut mismatches = Vec::new();
    for fixture in fixtures() {
        let source_text = fs::read_to_string(&fixture).unwrap_or_else(|error| {
            panic!("{}: failed to read fixture: {error}", fixture.display())
        });
        let mut source = Source::from_string(source_text.clone());
        let tokens = Lexer::new(&mut source)
            .lex_all()
            .unwrap_or_else(|error| panic!("{}: legacy lexer failed: {error}", fixture.display()));
        let legacy = Parser::new(&tokens).parse().unwrap_or_else(|(_, errors)| {
            panic!("{}: legacy parser failed: {errors:#?}", fixture.display())
        });

        let parsed = syntax::parse_source(&source_text);
        assert!(
            parsed.errors.is_empty(),
            "{}: syntax frontend errors: {:#?}",
            fixture.display(),
            parsed.errors
        );
        let lowered = parsed.lower().unwrap_or_else(|error| {
            panic!("{}: syntax lowering failed: {error}", fixture.display())
        });

        let legacy_spans = collect_ast(&legacy);
        let syntax_spans = collect_ast(&lowered);
        let location_source = Source::from_string(source_text.clone());
        assert_eq!(
            legacy_spans.keys().collect::<Vec<_>>(),
            syntax_spans.keys().collect::<Vec<_>>(),
            "{}: span-bearing AST paths differ",
            fixture.display()
        );
        for (path, legacy_span) in legacy_spans {
            mismatches.extend(location_mismatches(
                &fixture,
                &path,
                legacy_span,
                syntax_spans[&path],
                &location_source,
            ));
            mismatches.extend(range_location_mismatches(
                &fixture,
                &path,
                syntax_spans[&path],
                &location_source,
            ));
        }
    }
    let failure_count = mismatches.len();
    assert!(
        mismatches.is_empty(),
        "location parity failures ({failure_count}; showing at most 250):\n{}",
        mismatches
            .iter()
            .take(250)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
