//! Rust parser adapter backed by tree-sitter.

use std::collections::{HashMap, HashSet};

use codeatlas_core::{
    EntryPointKind, Language, ParseInput, ParsedCall, ParsedEntryPoint, ParsedFile, ParsedImport,
    ParsedModule, ParsedReference, ParsedSymbol, ParsedSymbolId, ParsedTarget, ParserAdapter,
    ParserError, ReferenceKind, RepositoryPath, SourceSpan, SymbolKind,
};
use tree_sitter::{Node, Parser, Point};

#[derive(Debug, Clone, Copy, Default)]
pub struct RustParser;

pub type RustParserAdapter = RustParser;

impl RustParser {
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl ParserAdapter for RustParser {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedFile, ParserError> {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_rust::LANGUAGE.into())
            .map_err(|error| {
                parser_error(input.path, format!("failed to load grammar: {error}"))
            })?;
        let tree = parser
            .parse(input.source, None)
            .ok_or_else(|| parser_error(input.path, "tree-sitter returned no syntax tree"))?;
        let root = tree.root_node();
        if root.has_error() {
            return Err(parser_error(
                input.path,
                "source contains Rust syntax errors",
            ));
        }

        let module =
            file_module(input.path, root).map_err(|message| parser_error(input.path, message))?;
        let mut parsed = ParsedFile::empty(input.path.clone(), Language::Rust);
        parsed.module = Some(module);
        if input.path.as_str().rsplit('/').next() == Some("lib.rs") {
            parsed.entry_points.push(ParsedEntryPoint {
                kind: EntryPointKind::Library,
                label: "library crate".to_owned(),
                symbol_id: None,
                span: node_span(root).map_err(|message| parser_error(input.path, message))?,
            });
        }

        let mut extractor = Extractor {
            source: input.source,
            parsed,
            definition_nodes: HashSet::new(),
            context: Vec::new(),
        };
        extractor
            .visit(root, None, &[])
            .map_err(|message| parser_error(input.path, message))?;
        extractor.resolve_local_targets();
        Ok(extractor.parsed)
    }
}

struct Extractor<'source> {
    source: &'source str,
    parsed: ParsedFile,
    definition_nodes: HashSet<usize>,
    context: Vec<String>,
}

impl Extractor<'_> {
    fn visit(
        &mut self,
        node: Node<'_>,
        enclosing_symbol: Option<ParsedSymbolId>,
        attributes: &[String],
    ) -> Result<(), String> {
        if node.kind() == "use_declaration" {
            self.extract_import(node)?;
            return Ok(());
        }
        if node.kind() == "attribute_item" {
            return Ok(());
        }

        let mut child_enclosing_symbol = enclosing_symbol;
        let mut pushed_context = false;
        if let Some(descriptor) = self.symbol_descriptor(node)? {
            let local_id = ParsedSymbolId(
                u32::try_from(self.parsed.symbols.len())
                    .map_err(|_| "Rust file contains more than u32::MAX symbols".to_owned())?,
            );
            if let Some(name_node) = descriptor.name_node {
                self.definition_nodes.insert(name_node.id());
            }
            let qualified_name = self.qualified_name(&descriptor.name);
            let symbol = ParsedSymbol {
                local_id,
                name: descriptor.name.clone(),
                qualified_name,
                kind: descriptor.kind.clone(),
                span: node_span(node)?,
                parent_id: enclosing_symbol,
            };
            self.extract_entry_point(node, &symbol, attributes)?;
            self.parsed.symbols.push(symbol);
            self.context.push(descriptor.context_name);
            pushed_context = true;
            child_enclosing_symbol = Some(local_id);
        }

        if node.kind() == "call_expression" {
            self.extract_call(node, child_enclosing_symbol)?;
        } else if node.kind() == "identifier" && !self.definition_nodes.contains(&node.id()) {
            let name = node_text(node, self.source)?.to_owned();
            self.parsed.references.push(ParsedReference {
                source_id: child_enclosing_symbol,
                target: ParsedTarget::Unresolved(name),
                kind: ReferenceKind::Read,
                span: node_span(node)?,
            });
        }

        let mut cursor = node.walk();
        let mut pending_attributes = Vec::new();
        for child in node.named_children(&mut cursor) {
            if child.kind() == "attribute_item" {
                pending_attributes.push(node_text(child, self.source)?.to_owned());
                continue;
            }
            if matches!(child.kind(), "line_comment" | "block_comment") {
                continue;
            }
            self.visit(child, child_enclosing_symbol, &pending_attributes)?;
            pending_attributes.clear();
        }

        if pushed_context {
            self.context.pop();
        }
        Ok(())
    }

    fn symbol_descriptor<'tree>(
        &self,
        node: Node<'tree>,
    ) -> Result<Option<SymbolDescriptor<'tree>>, String> {
        let (kind, name_node) = match node.kind() {
            "mod_item" => (SymbolKind::Module, definition_name(node)),
            "function_item" => {
                let kind = if has_ancestor_kind(node, "impl_item")
                    || has_ancestor_kind(node, "trait_item")
                {
                    SymbolKind::Method
                } else {
                    SymbolKind::Function
                };
                (kind, definition_name(node))
            }
            "function_signature_item" => (SymbolKind::Method, definition_name(node)),
            "struct_item" => (SymbolKind::Struct, definition_name(node)),
            "enum_item" => (SymbolKind::Enum, definition_name(node)),
            "trait_item" => (SymbolKind::Trait, definition_name(node)),
            "type_item" | "associated_type" => (SymbolKind::TypeAlias, definition_name(node)),
            "const_item" => (SymbolKind::Constant, definition_name(node)),
            "static_item" => (SymbolKind::Static, definition_name(node)),
            "macro_definition" => (SymbolKind::Macro, definition_name(node)),
            "impl_item" => {
                let name = impl_name(node, self.source)?;
                return Ok(Some(SymbolDescriptor {
                    context_name: name.clone(),
                    name,
                    kind: SymbolKind::Other("impl".to_owned()),
                    name_node: None,
                }));
            }
            _ => return Ok(None),
        };

        let name_node = name_node.ok_or_else(|| {
            format!(
                "{} at byte {} has no definition name",
                node.kind(),
                node.start_byte()
            )
        })?;
        let name = node_text(name_node, self.source)?.to_owned();
        Ok(Some(SymbolDescriptor {
            context_name: name.clone(),
            name,
            kind,
            name_node: Some(name_node),
        }))
    }

    fn qualified_name(&self, name: &str) -> String {
        let module = self
            .parsed
            .module
            .as_ref()
            .expect("Rust parser always creates a file module");
        let mut qualified = module.qualified_name.clone();
        for context in &self.context {
            qualified.push_str("::");
            qualified.push_str(context);
        }
        qualified.push_str("::");
        qualified.push_str(name);
        qualified
    }

    fn extract_import(&mut self, node: Node<'_>) -> Result<(), String> {
        let argument = node
            .child_by_field_name("argument")
            .or_else(|| last_import_child(node))
            .ok_or_else(|| {
                format!(
                    "use declaration at byte {} has no target",
                    node.start_byte()
                )
            })?;
        let raw_target = node_text(argument, self.source)?.trim();
        let (target, alias) = split_import_alias(raw_target);
        self.parsed.imports.push(ParsedImport {
            target: target.to_owned(),
            span: node_span(node)?,
            alias: alias.map(str::to_owned),
        });
        Ok(())
    }

    fn extract_call(
        &mut self,
        node: Node<'_>,
        caller_id: Option<ParsedSymbolId>,
    ) -> Result<(), String> {
        let function = node
            .child_by_field_name("function")
            .or_else(|| node.named_child(0))
            .ok_or_else(|| format!("call at byte {} has no function", node.start_byte()))?;
        let target = node_text(function, self.source)?.trim().to_owned();
        self.parsed.calls.push(ParsedCall {
            caller_id,
            target: ParsedTarget::Unresolved(target),
            span: node_span(node)?,
            confidence: None,
        });
        Ok(())
    }

    fn extract_entry_point(
        &mut self,
        node: Node<'_>,
        symbol: &ParsedSymbol,
        attributes: &[String],
    ) -> Result<(), String> {
        if !matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
            return Ok(());
        }
        let name_node = definition_name(node).ok_or_else(|| {
            format!(
                "function at byte {} has no definition name",
                node.start_byte()
            )
        })?;
        let prefix = self
            .source
            .get(node.start_byte()..name_node.start_byte())
            .ok_or_else(|| "function attribute range is not valid UTF-8".to_owned())?;
        let mut compact = attributes.concat();
        compact.push_str(prefix);
        let compact = compact
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect::<String>();
        let kind = if has_attribute(&compact, "bench") {
            Some(EntryPointKind::Benchmark)
        } else if has_attribute(&compact, "test") {
            Some(EntryPointKind::Test)
        } else if symbol.kind == SymbolKind::Function
            && (symbol.name == "main" || has_attribute(&compact, "main"))
        {
            Some(EntryPointKind::Executable)
        } else {
            None
        };
        if let Some(kind) = kind {
            self.parsed.entry_points.push(ParsedEntryPoint {
                kind,
                label: symbol.name.clone(),
                symbol_id: Some(symbol.local_id),
                span: symbol.span,
            });
        }
        Ok(())
    }

    fn resolve_local_targets(&mut self) {
        let mut all_symbols = HashMap::<String, Vec<ParsedSymbolId>>::new();
        let mut callable_symbols = HashMap::<String, Vec<ParsedSymbolId>>::new();
        for symbol in &self.parsed.symbols {
            all_symbols
                .entry(symbol.name.clone())
                .or_default()
                .push(symbol.local_id);
            if matches!(symbol.kind, SymbolKind::Function | SymbolKind::Method) {
                callable_symbols
                    .entry(symbol.name.clone())
                    .or_default()
                    .push(symbol.local_id);
            }
        }
        for reference in &mut self.parsed.references {
            resolve_simple_target(&mut reference.target, &all_symbols);
        }
        for call in &mut self.parsed.calls {
            resolve_simple_target(&mut call.target, &callable_symbols);
        }
    }
}

struct SymbolDescriptor<'tree> {
    name: String,
    context_name: String,
    kind: SymbolKind,
    name_node: Option<Node<'tree>>,
}

fn file_module(path: &RepositoryPath, root: Node<'_>) -> Result<ParsedModule, String> {
    let mut components = path.as_str().split('/').collect::<Vec<_>>();
    let file_name = components
        .pop()
        .ok_or_else(|| "Rust repository path has no file name".to_owned())?;
    let stem = file_name
        .strip_suffix(".rs")
        .ok_or_else(|| "Rust repository path does not end in .rs".to_owned())?;
    if components.first() == Some(&"src") {
        components.remove(0);
    }
    if !matches!(stem, "lib" | "main" | "mod") {
        components.push(stem);
    }
    let mut qualified_name = "crate".to_owned();
    for component in &components {
        qualified_name.push_str("::");
        qualified_name.push_str(component);
    }
    let name = components.last().copied().unwrap_or("crate").to_owned();
    Ok(ParsedModule {
        name,
        qualified_name,
        span: Some(node_span(root)?),
    })
}

fn definition_name(node: Node<'_>) -> Option<Node<'_>> {
    node.child_by_field_name("name").or_else(|| {
        let mut cursor = node.walk();
        node.named_children(&mut cursor)
            .find(|child| matches!(child.kind(), "identifier" | "type_identifier"))
    })
}

fn last_import_child(node: Node<'_>) -> Option<Node<'_>> {
    let mut cursor = node.walk();
    node.named_children(&mut cursor)
        .filter(|child| !matches!(child.kind(), "visibility_modifier" | "attribute_item"))
        .last()
}

fn impl_name(node: Node<'_>, source: &str) -> Result<String, String> {
    let end = node
        .child_by_field_name("body")
        .map_or_else(|| node.end_byte(), |body| body.start_byte());
    let header = source
        .get(node.start_byte()..end)
        .ok_or_else(|| "impl header is not a valid UTF-8 range".to_owned())?;
    Ok(header.split_whitespace().collect::<Vec<_>>().join(" "))
}

fn has_ancestor_kind(mut node: Node<'_>, expected: &str) -> bool {
    while let Some(parent) = node.parent() {
        if parent.kind() == expected {
            return true;
        }
        if matches!(parent.kind(), "source_file" | "mod_item") {
            return false;
        }
        node = parent;
    }
    false
}

fn split_import_alias(target: &str) -> (&str, Option<&str>) {
    if target.contains('{') {
        return (target, None);
    }
    target
        .rsplit_once(" as ")
        .map_or((target, None), |(target, alias)| {
            (target.trim(), Some(alias.trim()))
        })
}

fn has_attribute(compact_prefix: &str, name: &str) -> bool {
    compact_prefix.contains(&format!("#[{name}]"))
        || compact_prefix.contains(&format!("::{name}]"))
        || compact_prefix.contains(&format!("::{name}("))
}

fn resolve_simple_target(
    target: &mut ParsedTarget,
    symbols: &HashMap<String, Vec<ParsedSymbolId>>,
) {
    let ParsedTarget::Unresolved(name) = target else {
        return;
    };
    if !is_simple_identifier(name) {
        return;
    }
    let Some(candidates) = symbols.get(name) else {
        return;
    };
    let [local_id] = candidates.as_slice() else {
        return;
    };
    *target = ParsedTarget::Local(*local_id);
}

fn is_simple_identifier(name: &str) -> bool {
    let name = name.strip_prefix("r#").unwrap_or(name);
    !name.is_empty()
        && name
            .chars()
            .all(|character| character == '_' || character.is_alphanumeric())
}

fn node_text<'source>(node: Node<'_>, source: &'source str) -> Result<&'source str, String> {
    source
        .get(node.start_byte()..node.end_byte())
        .ok_or_else(|| format!("{} node is not a valid UTF-8 range", node.kind()))
}

fn node_span(node: Node<'_>) -> Result<SourceSpan, String> {
    span_from_points(node.start_position(), node.end_position())
}

fn span_from_points(start: Point, end: Point) -> Result<SourceSpan, String> {
    let start_line = u32::try_from(start.row)
        .ok()
        .and_then(|line| line.checked_add(1))
        .ok_or_else(|| "source start line exceeds u32::MAX".to_owned())?;
    let end_line = u32::try_from(end.row)
        .ok()
        .and_then(|line| line.checked_add(1))
        .ok_or_else(|| "source end line exceeds u32::MAX".to_owned())?;
    let start_column = u32::try_from(start.column)
        .map_err(|_| "source start column exceeds u32::MAX".to_owned())?;
    let end_column =
        u32::try_from(end.column).map_err(|_| "source end column exceeds u32::MAX".to_owned())?;
    SourceSpan::new(start_line, start_column, end_line, end_column)
        .map_err(|error| error.to_string())
}

fn parser_error(path: &RepositoryPath, message: impl Into<String>) -> ParserError {
    ParserError {
        path: path.clone(),
        message: message.into(),
    }
}
