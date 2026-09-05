//! Tree-sitter based Python parser adapter for `CodeAtlas`.

use std::collections::{BTreeMap, BTreeSet};

use codeatlas_core::{
    EntryPointKind, Language, ParseInput, ParsedCall, ParsedEntryPoint, ParsedFile, ParsedImport,
    ParsedModule, ParsedReference, ParsedSymbol, ParsedSymbolId, ParsedTarget, ParserAdapter,
    ParserError, ReferenceKind, RepositoryPath, SourceSpan, SymbolKind,
};
use tree_sitter::{Node, Parser};

const PYTHON_PATH_ERROR: &str = "expected a .py or .pyi repository path";

/// Parses Python source files into `CodeAtlas`'s language-neutral parser IR.
#[derive(Debug, Default, Clone, Copy)]
pub struct PythonParser;

/// Explicit adapter-style alias for [`PythonParser`].
pub type PythonParserAdapter = PythonParser;

impl PythonParser {
    /// Creates a Python parser adapter.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }

    /// Returns whether `path` has a supported `.py` or `.pyi` suffix.
    #[must_use]
    pub fn supports_path(path: &RepositoryPath) -> bool {
        strip_python_suffix(path.as_str()).is_some()
    }
}

impl ParserAdapter for PythonParser {
    fn language(&self) -> Language {
        Language::Python
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedFile, ParserError> {
        if !Self::supports_path(input.path) {
            return Err(parse_error(input.path, PYTHON_PATH_ERROR));
        }

        let (module_name, qualified_module_name) =
            module_names(input.path).map_err(|message| parse_error(input.path, &message))?;
        let mut parser = Parser::new();
        let language = tree_sitter_python::LANGUAGE.into();
        parser.set_language(&language).map_err(|error| {
            parse_error(
                input.path,
                &format!("could not load the Python grammar: {error}"),
            )
        })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| parse_error(input.path, "tree-sitter did not produce a syntax tree"))?;
        let root = tree.root_node();
        let root_span = node_span(root).map_err(|message| parse_error(input.path, &message))?;

        let mut extractor = Extractor::new(input.source, &qualified_module_name);
        extractor
            .collect_symbols(root, &mut Vec::new())
            .map_err(|message| parse_error(input.path, &message))?;
        extractor
            .collect_records(root, &mut Vec::new())
            .map_err(|message| parse_error(input.path, &message))?;
        extractor.add_test_entry_points();
        extractor.sort_records();

        Ok(ParsedFile {
            path: input.path.clone(),
            language: Language::Python,
            module: Some(ParsedModule {
                name: module_name,
                qualified_name: qualified_module_name.clone(),
                span: Some(root_span),
            }),
            symbols: extractor.symbols,
            imports: extractor.imports,
            references: extractor.references,
            calls: extractor.calls,
            entry_points: extractor.entry_points,
        })
    }
}

fn parse_error(path: &RepositoryPath, message: &str) -> ParserError {
    ParserError {
        path: path.clone(),
        message: message.to_owned(),
    }
}

fn module_names(path: &RepositoryPath) -> Result<(String, String), String> {
    let stem = strip_python_suffix(path.as_str()).ok_or_else(|| PYTHON_PATH_ERROR.to_owned())?;
    let mut components: Vec<&str> = stem.split('/').collect();
    if components.last() == Some(&"") {
        return Err(format!("cannot derive a Python module from {path}"));
    }
    if components.len() > 1 && components.last() == Some(&"__init__") {
        components.pop();
    }

    let name = components
        .last()
        .ok_or_else(|| format!("cannot derive a Python module from {path}"))?
        .to_string();
    Ok((name, components.join(".")))
}

fn strip_python_suffix(path: &str) -> Option<&str> {
    let (stem, extension) = path.rsplit_once('.')?;
    (extension.eq_ignore_ascii_case("py") || extension.eq_ignore_ascii_case("pyi")).then_some(stem)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScopeKind {
    Function,
    Class,
}

#[derive(Debug, Clone, Copy)]
struct ActiveScope {
    id: ParsedSymbolId,
    symbol_index: usize,
    kind: ScopeKind,
}

struct Extractor<'source> {
    source: &'source str,
    module_name: &'source str,
    symbols: Vec<ParsedSymbol>,
    scopes_by_node: BTreeMap<usize, ActiveScope>,
    constant_keys: BTreeSet<(Option<ParsedSymbolId>, String)>,
    unittest_classes: BTreeSet<ParsedSymbolId>,
    imports: Vec<ParsedImport>,
    references: Vec<ParsedReference>,
    calls: Vec<ParsedCall>,
    entry_points: Vec<ParsedEntryPoint>,
}

impl<'source> Extractor<'source> {
    fn new(source: &'source str, module_name: &'source str) -> Self {
        Self {
            source,
            module_name,
            symbols: Vec::new(),
            scopes_by_node: BTreeMap::new(),
            constant_keys: BTreeSet::new(),
            unittest_classes: BTreeSet::new(),
            imports: Vec::new(),
            references: Vec::new(),
            calls: Vec::new(),
            entry_points: Vec::new(),
        }
    }

    fn collect_symbols(
        &mut self,
        node: Node<'_>,
        scopes: &mut Vec<ActiveScope>,
    ) -> Result<(), String> {
        let scope = match node.kind() {
            "function_definition" => self.add_function(node, scopes)?,
            "class_definition" => self.add_class(node, scopes)?,
            "assignment" => {
                self.add_constant(node, scopes)?;
                None
            }
            _ => None,
        };

        if let Some(scope) = scope {
            scopes.push(scope);
        }
        self.visit_named_children(node, |extractor, child| {
            extractor.collect_symbols(child, scopes)
        })?;
        if scope.is_some() {
            scopes.pop();
        }
        Ok(())
    }

    fn add_function(
        &mut self,
        node: Node<'_>,
        scopes: &[ActiveScope],
    ) -> Result<Option<ActiveScope>, String> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let name = self.node_text(name_node)?.to_owned();
        let kind = if scopes
            .last()
            .is_some_and(|scope| scope.kind == ScopeKind::Class)
        {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        self.add_scoped_symbol(node, scopes, name, kind, ScopeKind::Function)
            .map(Some)
    }

    fn add_class(
        &mut self,
        node: Node<'_>,
        scopes: &[ActiveScope],
    ) -> Result<Option<ActiveScope>, String> {
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let name = self.node_text(name_node)?.to_owned();
        let scope =
            self.add_scoped_symbol(node, scopes, name, SymbolKind::Class, ScopeKind::Class)?;
        if self.has_test_case_base(node)? {
            self.unittest_classes.insert(scope.id);
        }
        Ok(Some(scope))
    }

    fn add_scoped_symbol(
        &mut self,
        node: Node<'_>,
        scopes: &[ActiveScope],
        name: String,
        kind: SymbolKind,
        scope_kind: ScopeKind,
    ) -> Result<ActiveScope, String> {
        let symbol_index = self.symbols.len();
        let local_id = local_id(symbol_index)?;
        let parent_id = scopes.last().map(|scope| scope.id);
        let qualified_name = self.qualify(&name, scopes);
        self.symbols.push(ParsedSymbol {
            local_id,
            name,
            qualified_name,
            kind,
            span: node_span(node)?,
            parent_id,
        });
        let scope = ActiveScope {
            id: local_id,
            symbol_index,
            kind: scope_kind,
        };
        self.scopes_by_node.insert(node.id(), scope);
        Ok(scope)
    }

    fn add_constant(&mut self, node: Node<'_>, scopes: &[ActiveScope]) -> Result<(), String> {
        if scopes
            .last()
            .is_some_and(|scope| scope.kind == ScopeKind::Function)
        {
            return Ok(());
        }
        let Some(left) = node.child_by_field_name("left") else {
            return Ok(());
        };
        if left.kind() != "identifier" {
            return Ok(());
        }
        let name = self.node_text(left)?;
        let explicit_final = node
            .child_by_field_name("type")
            .is_some_and(|annotation| self.is_final_annotation(annotation));
        if !is_constant_name(name) && !explicit_final {
            return Ok(());
        }

        let parent_id = scopes.last().map(|scope| scope.id);
        if !self.constant_keys.insert((parent_id, name.to_owned())) {
            return Ok(());
        }
        let local_id = local_id(self.symbols.len())?;
        self.symbols.push(ParsedSymbol {
            local_id,
            name: name.to_owned(),
            qualified_name: self.qualify(name, scopes),
            kind: SymbolKind::Constant,
            span: node_span(node)?,
            parent_id,
        });
        Ok(())
    }

    fn qualify(&self, name: &str, scopes: &[ActiveScope]) -> String {
        let prefix = scopes.last().map_or(self.module_name, |scope| {
            self.symbols[scope.symbol_index].qualified_name.as_str()
        });
        format!("{prefix}.{name}")
    }

    fn is_final_annotation(&self, annotation: Node<'_>) -> bool {
        self.node_text(annotation).is_ok_and(|text| {
            let compact: String = text
                .chars()
                .filter(|character| !character.is_whitespace())
                .collect();
            compact == "Final"
                || compact.starts_with("Final[")
                || compact.ends_with(".Final")
                || compact.contains(".Final[")
        })
    }

    fn has_test_case_base(&self, class: Node<'_>) -> Result<bool, String> {
        let Some(arguments) = class.child_by_field_name("superclasses") else {
            return Ok(false);
        };
        for base in named_children(arguments) {
            if base.kind() == "keyword_argument" {
                continue;
            }
            let reference = inheritance_reference_node(base);
            let name = self.reference_name(reference)?;
            if name == "TestCase" || name.ends_with(".TestCase") {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn collect_records(
        &mut self,
        node: Node<'_>,
        scopes: &mut Vec<ActiveScope>,
    ) -> Result<(), String> {
        let scope = self.scopes_by_node.get(&node.id()).copied();
        if let Some(scope) = scope {
            scopes.push(scope);
        }

        match node.kind() {
            "import_statement" => self.add_import_statement(node)?,
            "import_from_statement" => self.add_from_import(node)?,
            "future_import_statement" => self.add_future_import(node)?,
            "call" => self.add_call(node, scopes)?,
            "class_definition" => self.add_inheritance_references(node, scopes)?,
            "if_statement" if scopes.is_empty() => self.add_main_guard(node)?,
            _ => {}
        }

        self.visit_named_children_with_fields(node, |extractor, child, field| {
            if matches!(field, Some("type" | "return_type")) {
                extractor.add_type_references(child, scopes)?;
            }
            extractor.collect_records(child, scopes)
        })?;
        if scope.is_some() {
            scopes.pop();
        }
        Ok(())
    }

    fn add_import_statement(&mut self, node: Node<'_>) -> Result<(), String> {
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let (target, alias) = self.import_item(item)?;
            self.imports.push(ParsedImport {
                target,
                span: node_span(item)?,
                alias,
            });
        }
        Ok(())
    }

    fn add_from_import(&mut self, node: Node<'_>) -> Result<(), String> {
        let Some(module) = node.child_by_field_name("module_name") else {
            return Ok(());
        };
        let module_name = self.node_text(module)?.to_owned();
        let mut found_item = false;
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            found_item = true;
            let (name, alias) = self.import_item(item)?;
            self.imports.push(ParsedImport {
                target: join_import_target(&module_name, &name),
                span: node_span(item)?,
                alias,
            });
        }
        if !found_item {
            self.add_wildcard_import(node, &module_name)?;
        }
        Ok(())
    }

    fn add_future_import(&mut self, node: Node<'_>) -> Result<(), String> {
        let mut cursor = node.walk();
        for item in node.children_by_field_name("name", &mut cursor) {
            let (name, alias) = self.import_item(item)?;
            self.imports.push(ParsedImport {
                target: join_import_target("__future__", &name),
                span: node_span(item)?,
                alias,
            });
        }
        Ok(())
    }

    fn add_wildcard_import(&mut self, node: Node<'_>, module: &str) -> Result<(), String> {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if matches!(child.kind(), "*" | "wildcard_import") {
                self.imports.push(ParsedImport {
                    target: join_import_target(module, "*"),
                    span: node_span(child)?,
                    alias: None,
                });
                break;
            }
        }
        Ok(())
    }

    fn import_item(&self, item: Node<'_>) -> Result<(String, Option<String>), String> {
        if item.kind() != "aliased_import" {
            return Ok((self.node_text(item)?.to_owned(), None));
        }
        let Some(name) = item.child_by_field_name("name") else {
            return Ok((self.node_text(item)?.to_owned(), None));
        };
        let alias = item
            .child_by_field_name("alias")
            .map(|node| self.node_text(node).map(str::to_owned))
            .transpose()?;
        Ok((self.node_text(name)?.to_owned(), alias))
    }

    fn add_call(&mut self, node: Node<'_>, scopes: &[ActiveScope]) -> Result<(), String> {
        let Some(function) = node.child_by_field_name("function") else {
            return Ok(());
        };
        let name = self.reference_name(function)?;
        let target = if function.kind() == "identifier" {
            self.resolve_local(&name, scopes, true)
                .map_or_else(|| ParsedTarget::Unresolved(name), ParsedTarget::Local)
        } else {
            ParsedTarget::Unresolved(name)
        };
        self.calls.push(ParsedCall {
            caller_id: scopes.last().map(|scope| scope.id),
            target,
            span: node_span(node)?,
            confidence: None,
        });
        Ok(())
    }

    fn add_inheritance_references(
        &mut self,
        node: Node<'_>,
        scopes: &[ActiveScope],
    ) -> Result<(), String> {
        let Some(arguments) = node.child_by_field_name("superclasses") else {
            return Ok(());
        };
        for base in named_children(arguments) {
            if base.kind() == "keyword_argument" {
                continue;
            }
            let reference = inheritance_reference_node(base);
            self.add_reference(reference, scopes, ReferenceKind::Inheritance)?;
        }
        Ok(())
    }

    fn add_type_references(
        &mut self,
        annotation: Node<'_>,
        scopes: &[ActiveScope],
    ) -> Result<(), String> {
        let mut atoms = Vec::new();
        collect_annotation_atoms(annotation, &mut atoms);
        for atom in atoms {
            self.add_reference(atom, scopes, ReferenceKind::Type)?;
        }
        Ok(())
    }

    fn add_reference(
        &mut self,
        node: Node<'_>,
        scopes: &[ActiveScope],
        kind: ReferenceKind,
    ) -> Result<(), String> {
        let name = self.reference_name(node)?;
        let target = if node.kind() == "identifier" {
            self.resolve_local(&name, scopes, false)
                .map_or_else(|| ParsedTarget::Unresolved(name), ParsedTarget::Local)
        } else {
            ParsedTarget::Unresolved(name)
        };
        self.references.push(ParsedReference {
            source_id: scopes.last().map(|scope| scope.id),
            target,
            kind,
            span: node_span(node)?,
        });
        Ok(())
    }

    fn resolve_local(
        &self,
        name: &str,
        scopes: &[ActiveScope],
        callable_only: bool,
    ) -> Option<ParsedSymbolId> {
        let mut crossed_function = false;
        for scope in scopes.iter().rev() {
            let visible = match scope.kind {
                ScopeKind::Function => {
                    crossed_function = true;
                    true
                }
                ScopeKind::Class => !crossed_function,
            };
            if visible && let Some(id) = self.find_symbol(name, Some(scope.id), callable_only) {
                return Some(id);
            }
        }
        self.find_symbol(name, None, callable_only)
    }

    fn find_symbol(
        &self,
        name: &str,
        parent_id: Option<ParsedSymbolId>,
        callable_only: bool,
    ) -> Option<ParsedSymbolId> {
        self.symbols
            .iter()
            .rev()
            .find(|symbol| {
                symbol.name == name
                    && symbol.parent_id == parent_id
                    && (!callable_only || is_callable(&symbol.kind))
            })
            .map(|symbol| symbol.local_id)
    }

    fn add_main_guard(&mut self, node: Node<'_>) -> Result<(), String> {
        let Some(condition) = node.child_by_field_name("condition") else {
            return Ok(());
        };
        if !self.is_main_condition(condition)? {
            return Ok(());
        }
        self.entry_points.push(ParsedEntryPoint {
            kind: EntryPointKind::Executable,
            label: "Python __main__ guard".to_owned(),
            symbol_id: None,
            span: node_span(node)?,
        });
        Ok(())
    }

    fn is_main_condition(&self, condition: Node<'_>) -> Result<bool, String> {
        let compact: String = self
            .node_text(condition)?
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect();
        let compact = trim_outer_parentheses(&compact);
        Ok(matches!(
            compact,
            "__name__==\"__main__\""
                | "__name__=='__main__'"
                | "\"__main__\"==__name__"
                | "'__main__'==__name__"
        ))
    }

    fn add_test_entry_points(&mut self) {
        let test_classes: BTreeSet<ParsedSymbolId> = self
            .symbols
            .iter()
            .filter(|symbol| {
                symbol.kind == SymbolKind::Class
                    && symbol.parent_id.is_none()
                    && (symbol.name.starts_with("Test")
                        || self.unittest_classes.contains(&symbol.local_id))
            })
            .map(|symbol| symbol.local_id)
            .collect();

        for symbol in &self.symbols {
            let is_test = match symbol.kind {
                SymbolKind::Class => test_classes.contains(&symbol.local_id),
                SymbolKind::Function => {
                    symbol.parent_id.is_none() && symbol.name.starts_with("test_")
                }
                SymbolKind::Method => {
                    symbol.name.starts_with("test_")
                        && symbol
                            .parent_id
                            .is_some_and(|id| test_classes.contains(&id))
                }
                _ => false,
            };
            if is_test {
                self.entry_points.push(ParsedEntryPoint {
                    kind: EntryPointKind::Test,
                    label: symbol.qualified_name.clone(),
                    symbol_id: Some(symbol.local_id),
                    span: symbol.span,
                });
            }
        }
    }

    fn reference_name(&self, node: Node<'_>) -> Result<String, String> {
        if node.kind() == "attribute" {
            if let Some(name) = self.dotted_attribute(node)? {
                return Ok(name);
            }
        }
        Ok(self.node_text(node)?.trim().to_owned())
    }

    fn dotted_attribute(&self, node: Node<'_>) -> Result<Option<String>, String> {
        if node.kind() == "identifier" {
            return Ok(Some(self.node_text(node)?.to_owned()));
        }
        if node.kind() != "attribute" {
            return Ok(None);
        }
        let Some(object) = node.child_by_field_name("object") else {
            return Ok(None);
        };
        let Some(attribute) = node.child_by_field_name("attribute") else {
            return Ok(None);
        };
        let Some(prefix) = self.dotted_attribute(object)? else {
            return Ok(None);
        };
        Ok(Some(format!("{prefix}.{}", self.node_text(attribute)?)))
    }

    fn sort_records(&mut self) {
        self.imports
            .sort_by_key(|record| span_sort_key(record.span));
        self.references
            .sort_by_key(|record| span_sort_key(record.span));
        self.calls.sort_by_key(|record| span_sort_key(record.span));
        self.entry_points.sort_by(|left, right| {
            span_sort_key(left.span)
                .cmp(&span_sort_key(right.span))
                .then_with(|| left.label.cmp(&right.label))
        });
    }

    fn visit_named_children<F>(&mut self, node: Node<'_>, mut visit: F) -> Result<(), String>
    where
        F: FnMut(&mut Self, Node<'_>) -> Result<(), String>,
    {
        for child in named_children(node) {
            visit(self, child)?;
        }
        Ok(())
    }

    fn visit_named_children_with_fields<F>(
        &mut self,
        node: Node<'_>,
        mut visit: F,
    ) -> Result<(), String>
    where
        F: FnMut(&mut Self, Node<'_>, Option<&'static str>) -> Result<(), String>,
    {
        for index in 0..node.named_child_count() {
            let Some(child) = node.named_child(index) else {
                continue;
            };
            let field_index = u32::try_from(index)
                .map_err(|_| "syntax node has too many children to index".to_owned())?;
            visit(self, child, node.field_name_for_named_child(field_index))?;
        }
        Ok(())
    }

    fn node_text(&self, node: Node<'_>) -> Result<&'source str, String> {
        node.utf8_text(self.source.as_bytes())
            .map_err(|error| format!("tree-sitter produced an invalid UTF-8 range: {error}"))
    }
}

fn named_children(node: Node<'_>) -> Vec<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor).collect()
}

fn inheritance_reference_node(node: Node<'_>) -> Node<'_> {
    match node.kind() {
        "subscript" => node.child_by_field_name("value").unwrap_or(node),
        "parenthesized_expression" => node.named_child(0).unwrap_or(node),
        _ => node,
    }
}

fn collect_annotation_atoms<'tree>(node: Node<'tree>, atoms: &mut Vec<Node<'tree>>) {
    match node.kind() {
        "identifier" | "none" | "attribute" | "member_type" => atoms.push(node),
        "string" | "concatenated_string" => {}
        _ => {
            for child in named_children(node) {
                collect_annotation_atoms(child, atoms);
            }
        }
    }
}

fn is_constant_name(name: &str) -> bool {
    name.chars().any(char::is_uppercase)
        && name
            .chars()
            .all(|character| character == '_' || character.is_uppercase() || character.is_numeric())
}

fn is_callable(kind: &SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Class
    )
}

fn join_import_target(module: &str, item: &str) -> String {
    if module.ends_with('.') {
        format!("{module}{item}")
    } else {
        format!("{module}.{item}")
    }
}

fn trim_outer_parentheses(mut value: &str) -> &str {
    while value.starts_with('(') && value.ends_with(')') {
        value = &value[1..value.len() - 1];
    }
    value
}

fn local_id(index: usize) -> Result<ParsedSymbolId, String> {
    u32::try_from(index)
        .map(ParsedSymbolId)
        .map_err(|_| "file contains more symbols than local IDs can represent".to_owned())
}

fn node_span(node: Node<'_>) -> Result<SourceSpan, String> {
    let start = node.start_position();
    let end = node.end_position();
    let start_line = point_line(start.row)?;
    let end_line = point_line(end.row)?;
    let start_column = u32::try_from(start.column)
        .map_err(|_| "source column exceeds the IR coordinate range".to_owned())?;
    let end_column = u32::try_from(end.column)
        .map_err(|_| "source column exceeds the IR coordinate range".to_owned())?;
    SourceSpan::new(start_line, start_column, end_line, end_column)
        .map_err(|error| format!("tree-sitter produced an invalid source span: {error}"))
}

fn point_line(row: usize) -> Result<u32, String> {
    u32::try_from(row)
        .ok()
        .and_then(|line| line.checked_add(1))
        .ok_or_else(|| "source line exceeds the IR coordinate range".to_owned())
}

fn span_sort_key(span: SourceSpan) -> (u32, u32, u32, u32) {
    (
        span.start().line(),
        span.start().column(),
        span.end().line(),
        span.end().column(),
    )
}
