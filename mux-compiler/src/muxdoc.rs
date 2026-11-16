//! Source-backed documentation comments shared by compiler and editor tools.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::diagnostic::{Diagnostic, DiagnosticCode, FileId, Files, Label};
use crate::lexer::{ByteRange, Span, TokenType};
use crate::syntax::{AstLoweringData, SyntaxElement, SyntaxNode, SyntaxTree};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationKind {
    Function,
    Type,
    Field,
    Variant,
    Variable,
    Test,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SectionKind {
    Parameter(String),
    Returns,
    Example,
    See,
    Since,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Section {
    pub kind: SectionKind,
    pub text: String,
    pub range: ByteRange,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocComment {
    pub range: ByteRange,
    pub summary: String,
    pub sections: Vec<Section>,
}

impl DocComment {
    #[must_use]
    pub fn markdown(&self) -> String {
        self.markdown_with_links(|_| None)
    }

    #[must_use]
    pub fn markdown_with_links(&self, resolve: impl Fn(&str) -> Option<String>) -> String {
        let mut output = self.summary.clone();
        for section in &self.sections {
            let body = section.text.trim();
            if !output.is_empty() {
                output.push_str("\n\n");
            }
            match &section.kind {
                SectionKind::Parameter(name) => output.push_str(&format!("**{name}:** {body}")),
                SectionKind::Returns => output.push_str(&format!("**Returns:** {body}")),
                SectionKind::Example => {
                    output.push_str("**Example**\n\n");
                    output.push_str(&fenced_mux(body));
                }
                SectionKind::See => {
                    if let Some(destination) = resolve(body) {
                        output.push_str(&format!("**See:** [{body}]({destination})"));
                    } else if let Some(url) = external_url(body) {
                        output.push_str(&format!("**See:** <{url}>"));
                    } else {
                        output.push_str(&format!("**See:** {body}"));
                    }
                }
                SectionKind::Since => output.push_str(&format!("**Since:** {body}")),
            }
        }
        output
    }

    #[must_use]
    pub fn parameter(&self, name: &str) -> Option<&str> {
        self.sections
            .iter()
            .find_map(|section| match &section.kind {
                SectionKind::Parameter(parameter) if parameter == name => {
                    Some(section.text.as_str())
                }
                _ => None,
            })
    }
}

#[derive(Debug, Clone)]
pub struct DocumentedDeclaration {
    pub name: String,
    pub qualified_name: String,
    pub kind: DeclarationKind,
    pub api: bool,
    pub range: ByteRange,
    pub name_range: ByteRange,
    pub signature_end: usize,
    pub signature_suffix: Option<ByteRange>,
    pub parameters: Vec<String>,
    pub returns_void: bool,
    pub doc: Option<DocComment>,
}

#[derive(Debug, Clone)]
pub struct DocProblem {
    pub code: DiagnosticCode,
    pub range: ByteRange,
    pub message: String,
}

impl DocProblem {
    #[must_use]
    pub fn to_diagnostic(&self, file_id: FileId) -> Diagnostic {
        Diagnostic::new(self.code)
            .with_message(&self.message)
            .with_file_id(file_id)
            .with_label(Label::primary(
                Span::new(self.range.start, self.range.end),
                &self.message,
            ))
    }
}

#[derive(Debug, Clone, Default)]
pub struct DocIndex {
    pub declarations: Vec<DocumentedDeclaration>,
    pub problems: Vec<DocProblem>,
}

pub enum ReferenceResolution<'a, T> {
    Found(&'a T),
    Missing,
    Ambiguous,
}

struct ReferenceEntry<T> {
    module: String,
    target: T,
    api: bool,
}

pub struct ReferenceIndex<T> {
    keys: HashMap<String, Vec<ReferenceEntry<T>>>,
}

impl<T> Default for ReferenceIndex<T> {
    fn default() -> Self {
        Self {
            keys: HashMap::new(),
        }
    }
}

impl<T> ReferenceIndex<T> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, module: &str, declaration: &DocumentedDeclaration, target: T)
    where
        T: Clone,
    {
        let short_module = module.rsplit('.').next().unwrap_or(module);
        for key in [
            declaration.name.clone(),
            declaration.qualified_name.clone(),
            format!("{short_module}.{}", declaration.qualified_name),
            format!("{module}.{}", declaration.qualified_name),
        ]
        .into_iter()
        .collect::<HashSet<_>>()
        {
            self.keys.entry(key).or_default().push(ReferenceEntry {
                module: module.to_owned(),
                target: target.clone(),
                api: declaration.api,
            });
        }
    }

    #[must_use]
    pub fn resolve(&self, module: &str, name: &str) -> ReferenceResolution<'_, T> {
        let Some(entries) = self.keys.get(name) else {
            return ReferenceResolution::Missing;
        };
        let local = entries
            .iter()
            .filter(|entry| entry.module == module)
            .collect::<Vec<_>>();
        let matches = if local.is_empty() {
            entries.iter().filter(|entry| entry.api).collect::<Vec<_>>()
        } else {
            local
        };
        match matches.as_slice() {
            [] => ReferenceResolution::Missing,
            [entry] => ReferenceResolution::Found(&entry.target),
            _ => ReferenceResolution::Ambiguous,
        }
    }
}

impl DocIndex {
    #[must_use]
    pub fn from_tree(tree: &SyntaxTree) -> Self {
        let mut index = Self::default();
        collect_declarations(
            tree.source(),
            tree.root(),
            &mut index.declarations,
            "",
            true,
        );
        let mut used = HashSet::new();
        for declaration in &mut index.declarations {
            declaration.doc = preceding_comment(tree, declaration.range.start, &mut used);
            if let Some(doc) = &declaration.doc {
                validate_comment(declaration, doc, &mut index.problems);
            }
        }
        for (position, token) in tree.tokens().iter().enumerate() {
            let text = token.text(tree.source());
            if is_doc_token(token.kind(), text) && !used.contains(&position) {
                index.problems.push(DocProblem {
                    code: DiagnosticCode::DocOrphan,
                    range: token.range(),
                    message: "MuxDoc comment is not attached to a declaration".into(),
                });
            }
        }
        index
    }

    #[must_use]
    pub fn before(&self, offset: usize) -> Option<&DocComment> {
        self.declarations
            .iter()
            .find(|declaration| {
                declaration.range.start == offset || declaration.name_range.start == offset
            })
            .and_then(|declaration| declaration.doc.as_ref())
    }

    fn validate_references(&mut self, references: &ReferenceIndex<()>, module: &str) {
        for declaration in &self.declarations {
            let Some(doc) = &declaration.doc else {
                continue;
            };
            for section in &doc.sections {
                let SectionKind::See = section.kind else {
                    continue;
                };
                let target = section.text.trim();
                if is_external_url(target) {
                    continue;
                }
                let (code, message) = match references.resolve(module, target) {
                    ReferenceResolution::Found(_) => continue,
                    ReferenceResolution::Ambiguous => (
                        DiagnosticCode::DocAmbiguousReference,
                        format!("MuxDoc reference '{target}' is ambiguous"),
                    ),
                    ReferenceResolution::Missing => (
                        DiagnosticCode::DocUnknownReference,
                        format!("MuxDoc reference '{target}' does not resolve"),
                    ),
                };
                self.problems.push(DocProblem {
                    code,
                    range: section.range,
                    message,
                });
            }
        }
    }
}

fn is_external_url(value: &str) -> bool {
    external_url(value).is_some()
}

fn external_url(value: &str) -> Option<String> {
    url::Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https"))
        .map(|url| url.to_string())
}

pub(crate) fn fenced_mux(body: &str) -> String {
    let longest_run = body.split(|ch| ch != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat((longest_run + 1).max(3));
    format!("{fence}mux\n{body}\n{fence}")
}

#[must_use]
pub fn diagnostics_for_files(files: &Files) -> Vec<Diagnostic> {
    let root_directory = files.iter().next().and_then(|(_, path, _)| {
        crate::analysis::absolute_path(path)
            .parent()
            .map(Path::to_path_buf)
    });
    let mut indexes = files
        .iter()
        .map(|(id, path, source)| {
            let parsed = crate::syntax::parse_source(source);
            (id, path.to_path_buf(), DocIndex::from_tree(&parsed.tree))
        })
        .collect::<Vec<_>>();
    let mut references = ReferenceIndex::new();
    for (_, path, index) in &indexes {
        let module = module_name(path, root_directory.as_deref());
        for declaration in &index.declarations {
            references.insert(&module, declaration, ());
        }
    }
    let mut diagnostics = Vec::new();
    for (id, path, index) in &mut indexes {
        if path.to_string_lossy().starts_with("<embedded>") {
            continue;
        }
        let module = module_name(path, root_directory.as_deref());
        index.validate_references(&references, &module);
        diagnostics.extend(
            index
                .problems
                .iter()
                .map(|problem| problem.to_diagnostic(*id)),
        );
    }
    diagnostics
}

pub fn module_name(path: &std::path::Path, root_directory: Option<&std::path::Path>) -> String {
    let relative = if let Ok(relative) = path.strip_prefix("<embedded>") {
        relative.to_path_buf()
    } else if let Some(root) = root_directory {
        crate::analysis::absolute_path(path)
            .strip_prefix(root)
            .map(Path::to_path_buf)
            .unwrap_or_else(|_| {
                path.file_name()
                    .map(PathBuf::from)
                    .unwrap_or_else(|| path.to_path_buf())
            })
    } else {
        path.to_path_buf()
    };
    relative
        .with_extension("")
        .to_string_lossy()
        .replace(['/', '\\'], ".")
}

fn collect_declarations(
    source: &str,
    node: &SyntaxNode,
    out: &mut Vec<DocumentedDeclaration>,
    parent: &str,
    api: bool,
) {
    let data = node.lowering_data();
    let (kind, name_range, parameters, returns_void) = match data {
        Some(AstLoweringData::Function {
            name,
            parameters,
            return_type,
            ..
        }) => {
            let names = parameters
                .iter()
                .filter_map(|range| {
                    find_node(node, *range).and_then(|parameter| match parameter.lowering_data() {
                        Some(AstLoweringData::Parameter { name, .. }) => {
                            source.get(name.start..name.end).map(str::to_owned)
                        }
                        _ => None,
                    })
                })
                .collect();
            (
                DeclarationKind::Function,
                *name,
                names,
                return_type.is_none_or(|range| {
                    source
                        .get(range.start..range.end)
                        .is_some_and(|text| text.trim() == "void")
                }),
            )
        }
        Some(
            AstLoweringData::Class { name, .. }
            | AstLoweringData::Interface { name, .. }
            | AstLoweringData::Enum { name, .. },
        ) => (DeclarationKind::Type, *name, Vec::new(), false),
        Some(AstLoweringData::Field { name, .. }) => {
            (DeclarationKind::Field, *name, Vec::new(), false)
        }
        Some(AstLoweringData::EnumVariant { name, .. }) => {
            (DeclarationKind::Variant, *name, Vec::new(), false)
        }
        Some(AstLoweringData::VariableDeclaration { name, .. }) => {
            (DeclarationKind::Variable, *name, Vec::new(), false)
        }
        Some(AstLoweringData::Test { name, .. }) => {
            (DeclarationKind::Test, *name, Vec::new(), false)
        }
        _ => {
            for child in node.children() {
                if let SyntaxElement::Node(child) = child {
                    collect_declarations(source, child, out, parent, api);
                }
            }
            return;
        }
    };
    let mut child_parent = parent.to_owned();
    let child_api = api && !matches!(kind, DeclarationKind::Function | DeclarationKind::Test);
    let signature_end = match data {
        Some(AstLoweringData::Function {
            body: Some(body), ..
        }) => body.start,
        Some(
            AstLoweringData::Class { .. }
            | AstLoweringData::Interface { .. }
            | AstLoweringData::Enum { .. },
        ) => node
            .children()
            .iter()
            .find_map(|child| match child {
                SyntaxElement::Node(child)
                    if matches!(
                        child.kind(),
                        crate::syntax::SyntaxKind::ClassBody
                            | crate::syntax::SyntaxKind::InterfaceBody
                            | crate::syntax::SyntaxKind::EnumBody
                    ) =>
                {
                    Some(child.range().start)
                }
                _ => None,
            })
            .unwrap_or(node.range().end),
        _ => node.range().end,
    };
    let signature_suffix = match data {
        Some(AstLoweringData::Class { where_clause, .. }) => *where_clause,
        _ => None,
    };
    if let Some(name) = source.get(name_range.start..name_range.end) {
        let qualified_name = if parent.is_empty() {
            name.to_owned()
        } else {
            format!("{parent}.{name}")
        };
        if matches!(
            kind,
            DeclarationKind::Type | DeclarationKind::Function | DeclarationKind::Test
        ) {
            child_parent = qualified_name.clone();
        }
        out.push(DocumentedDeclaration {
            name: name.to_owned(),
            qualified_name,
            kind,
            api,
            range: node.range(),
            name_range,
            signature_end,
            signature_suffix,
            parameters,
            returns_void,
            doc: None,
        });
    }
    for child in node.children() {
        if let SyntaxElement::Node(child) = child {
            collect_declarations(source, child, out, &child_parent, child_api);
        }
    }
}

fn find_node(node: &SyntaxNode, range: ByteRange) -> Option<&SyntaxNode> {
    if node.range() == range {
        return Some(node);
    }
    node.children().iter().find_map(|child| match child {
        SyntaxElement::Node(child) => find_node(child, range),
        SyntaxElement::Token(_) => None,
    })
}

fn is_doc_token(kind: &TokenType, text: &str) -> bool {
    matches!(kind, TokenType::LineComment(_)) && text.starts_with("///")
        || matches!(kind, TokenType::MultilineComment(_)) && text.starts_with("/**")
}

fn preceding_comment(
    tree: &SyntaxTree,
    start: usize,
    used: &mut HashSet<usize>,
) -> Option<DocComment> {
    let tokens = tree.tokens();
    let mut cursor = start;
    let mut pieces = Vec::new();
    let upper = tokens.partition_point(|token| token.range().end <= start);
    for (position, token) in tokens[..upper].iter().enumerate().rev() {
        let range = token.range();
        if range.end > cursor {
            continue;
        }
        if matches!(token.kind(), TokenType::Whitespace | TokenType::NewLine) {
            continue;
        }
        let gap = tree.source().get(range.end..cursor)?;
        if !gap.chars().all(char::is_whitespace) || gap.matches('\n').count() > 1 {
            break;
        }
        let text = token.text(tree.source());
        if !is_doc_token(token.kind(), text) {
            break;
        }
        if !pieces.is_empty() && !matches!(token.kind(), TokenType::LineComment(_)) {
            break;
        }
        used.insert(position);
        pieces.push((range, text));
        cursor = range.start;
        if matches!(token.kind(), TokenType::MultilineComment(_)) {
            break;
        }
    }
    if pieces.is_empty() {
        return None;
    }
    pieces.reverse();
    let range = ByteRange::new(pieces[0].0.start, pieces.last()?.0.end);
    let mut lines = Vec::new();
    for (piece_range, text) in pieces {
        if let Some(body) = text.strip_prefix("///") {
            lines.push((
                body.strip_prefix(' ').unwrap_or(body).to_owned(),
                piece_range,
            ));
        } else if let Some(body) = text
            .strip_prefix("/**")
            .and_then(|body| body.strip_suffix("*/"))
        {
            let mut line_start = piece_range.start + 3;
            for raw_line in body.split_inclusive('\n') {
                let line_end = line_start + raw_line.trim_end_matches(['\r', '\n']).len();
                let line = raw_line.trim_start();
                let line = line.strip_prefix('*').unwrap_or(line);
                lines.push((
                    line.strip_prefix(' ')
                        .unwrap_or(line)
                        .trim_end_matches(['\r', '\n'])
                        .to_owned(),
                    ByteRange::new(line_start, line_end),
                ));
                line_start += raw_line.len();
            }
        }
    }
    Some(parse_comment(range, &lines))
}

fn parse_comment(range: ByteRange, lines: &[(String, ByteRange)]) -> DocComment {
    let mut summary = Vec::new();
    let mut sections: Vec<Section> = Vec::new();
    for (line, line_range) in lines {
        let trimmed = line.trim();
        let heading = (line == line.trim_start())
            .then(|| trimmed.split_once(':'))
            .flatten()
            .filter(|(key, _)| {
                key.chars()
                    .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            });
        if let Some((key, value)) = heading {
            let kind = match key {
                "returns" => SectionKind::Returns,
                "example" => SectionKind::Example,
                "see" => SectionKind::See,
                "since" => SectionKind::Since,
                _ => SectionKind::Parameter(key.to_owned()),
            };
            sections.push(Section {
                kind,
                text: value.trim().to_owned(),
                range: *line_range,
            });
        } else if let Some(section) = sections.last_mut() {
            if !section.text.is_empty() {
                section.text.push('\n');
            }
            section.text.push_str(line);
        } else {
            summary.push(line.as_str());
        }
    }
    DocComment {
        range,
        summary: summary.join("\n").trim().to_owned(),
        sections,
    }
}

fn validate_comment(
    declaration: &DocumentedDeclaration,
    doc: &DocComment,
    problems: &mut Vec<DocProblem>,
) {
    let mut seen = HashSet::new();
    for section in &doc.sections {
        let reason = match &section.kind {
            SectionKind::Parameter(name) if declaration.kind != DeclarationKind::Function => Some(
                format!("parameter section '{name}' is only valid on a function"),
            ),
            SectionKind::Parameter(name) if !declaration.parameters.contains(name) => Some(
                format!("'{name}' is not a parameter of {}", declaration.name),
            ),
            SectionKind::Returns
                if declaration.kind != DeclarationKind::Function || declaration.returns_void =>
            {
                Some("returns section requires a function with a value return type".into())
            }
            _ if section.text.trim().is_empty() => Some("MuxDoc section cannot be empty".into()),
            _ => None,
        };
        let key = match &section.kind {
            SectionKind::Parameter(name) => Some(format!("param:{name}")),
            SectionKind::Returns => Some("returns".into()),
            SectionKind::Since => Some("since".into()),
            _ => None,
        };
        let reason = reason.or_else(|| {
            key.filter(|key| !seen.insert(key.clone()))
                .map(|key| format!("duplicate MuxDoc section '{key}'"))
        });
        if let Some(message) = reason {
            problems.push(DocProblem {
                code: DiagnosticCode::DocInvalidSection,
                range: section.range,
                message,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index(source: &str) -> DocIndex {
        let parsed = crate::syntax::parse_source(source);
        assert!(parsed.errors.is_empty(), "{:?}", parsed.errors);
        DocIndex::from_tree(&parsed.tree)
    }

    #[test]
    fn block_sections_attach_and_render() {
        let docs = index(
            "/**\n * Larger value.\n * a: First value\n * returns: The larger value\n * example:\n *   print(max(1, 2))\n * since: beta\n */\nfunc max(int a) returns int { return a }\n",
        );
        assert!(docs.problems.is_empty(), "{:?}", docs.problems);
        let comment = docs.declarations[0].doc.as_ref().unwrap();
        assert_eq!(comment.summary, "Larger value.");
        assert_eq!(comment.parameter("a"), Some("First value"));
        assert!(comment.markdown().contains("```mux\nprint(max(1, 2))\n```"));
    }

    #[test]
    fn line_docs_do_not_consume_ordinary_comments() {
        let docs = index(
            "// ordinary\nfunc first() returns void { return }\n/// Documented.\nfunc second() returns void { return }\n",
        );
        assert!(docs.declarations[0].doc.is_none());
        assert_eq!(
            docs.declarations[1].doc.as_ref().unwrap().summary,
            "Documented."
        );
    }

    #[test]
    fn invalid_and_detached_docs_warn() {
        let source = "/// missing: Unknown\nfunc run(int actual) returns void { return }\n/// Detached\n\nfunc later() returns void { return }\n";
        let docs = index(source);
        assert!(
            docs.problems
                .iter()
                .any(|problem| problem.code == DiagnosticCode::DocInvalidSection)
        );
        assert!(
            docs.problems
                .iter()
                .any(|problem| problem.code == DiagnosticCode::DocOrphan)
        );
        let mismatch = docs
            .problems
            .iter()
            .find(|problem| problem.code == DiagnosticCode::DocInvalidSection)
            .unwrap();
        assert_eq!(
            &source[mismatch.range.start..mismatch.range.end],
            "/// missing: Unknown"
        );
    }

    #[test]
    fn comments_attach_to_members_variants_and_constants() {
        let docs = index(
            "/// A value.\nconst int LIMIT = 3\n/// A point.\nclass Point {\n    /// Horizontal position.\n    int x\n    /// Read the position.\n    func get() returns int { return self.x }\n}\n/// A state.\nenum State {\n    /// Ready state.\n    Ready,\n    /// Waiting state.\n    Waiting\n}\n",
        );
        for name in [
            "LIMIT",
            "Point",
            "Point.x",
            "Point.get",
            "State",
            "State.Ready",
            "State.Waiting",
        ] {
            let declaration = docs
                .declarations
                .iter()
                .find(|declaration| declaration.qualified_name == name)
                .unwrap();
            assert!(declaration.doc.is_some(), "missing docs on {name}");
            assert!(declaration.api, "{name} should be part of the module API");
        }
        assert!(docs.problems.is_empty(), "{:?}", docs.problems);
    }

    #[test]
    fn examples_can_contain_markdown_fences() {
        let rendered = fenced_mux("```\nprint(1)\n```");
        assert!(rendered.starts_with("````mux\n"));
        assert!(rendered.ends_with("\n````"));
    }

    #[test]
    fn external_see_urls_render_as_autolinks() {
        let docs =
            index("/// see: https://example.com/path_(1)\nfunc f() returns void { return }\n");
        let comment = docs.declarations[0].doc.as_ref().unwrap();
        assert!(
            comment
                .markdown()
                .contains("**See:** <https://example.com/path_(1)>")
        );
    }

    #[test]
    fn see_references_resolve_across_local_modules() {
        let mut files = Files::new();
        files.add(
            "/tmp/muxdoc-see/main.mux",
            "/// see: math.min\nfunc main() returns void { return }\n".into(),
        );
        files.add(
            "/tmp/muxdoc-see/math.mux",
            "func min(int a, int b) returns int { if a < b { return a } return b }\n".into(),
        );
        assert!(
            !diagnostics_for_files(&files)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DocUnknownReference)
        );
        let mut broken = Files::new();
        broken.add(
            "/tmp/muxdoc-see/main.mux",
            "/// see: math.missing\nfunc main() returns void { return }\n".into(),
        );
        broken.add(
            "/tmp/muxdoc-see/math.mux",
            "func min(int a, int b) returns int { if a < b { return a } return b }\n".into(),
        );
        assert!(
            diagnostics_for_files(&broken)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DocUnknownReference)
        );
        let mut ambiguous = Files::new();
        ambiguous.add(
            "/tmp/muxdoc-see/main.mux",
            "/// see: min\nfunc main() returns void { return }\n".into(),
        );
        ambiguous.add(
            "/tmp/muxdoc-see/math.mux",
            "func min() returns void { return }\n".into(),
        );
        ambiguous.add(
            "/tmp/muxdoc-see/other.mux",
            "func min() returns void { return }\n".into(),
        );
        assert!(
            diagnostics_for_files(&ambiguous)
                .iter()
                .any(|diagnostic| diagnostic.code == DiagnosticCode::DocAmbiguousReference)
        );
    }
}
