use codeatlas_core::{
    EntryPointKind, ParseInput, ParsedTarget, ParserAdapter, RepositoryPath, SymbolKind,
};
use codeatlas_lang_rust::RustParser;

fn parse_fixture(path: &str, source: &str) -> codeatlas_core::ParsedFile {
    let path = RepositoryPath::new(path).expect("fixture path should be canonical");
    RustParser::new()
        .parse(ParseInput {
            path: &path,
            source,
        })
        .expect("fixture should parse")
}

#[test]
fn extracts_rust_symbols_imports_calls_and_entry_points() {
    let parsed = parse_fixture("src/main.rs", include_str!("fixtures/full.rs"));

    for (name, kind) in [
        ("nested", SymbolKind::Module),
        ("nested_helper", SymbolKind::Function),
        ("Widget", SymbolKind::Struct),
        ("State", SymbolKind::Enum),
        ("Runner", SymbolKind::Trait),
        ("run", SymbolKind::Method),
        ("WidgetAlias", SymbolKind::TypeAlias),
        ("LIMIT", SymbolKind::Constant),
        ("ENABLED", SymbolKind::Static),
        ("announce", SymbolKind::Macro),
    ] {
        assert!(
            parsed
                .symbols
                .iter()
                .any(|symbol| symbol.name == name && symbol.kind == kind),
            "missing {kind:?} {name}"
        );
    }
    assert!(
        parsed
            .symbols
            .iter()
            .any(|symbol| symbol.kind == SymbolKind::Other("impl".to_owned()))
    );
    assert_eq!(parsed.imports.len(), 1);
    assert_eq!(parsed.imports[0].target, "std::fmt::Debug");
    assert_eq!(parsed.imports[0].alias.as_deref(), Some("DebugTrait"));
    assert!(!parsed.references.is_empty());
    assert!(
        parsed
            .references
            .iter()
            .any(|reference| matches!(reference.target, ParsedTarget::Local(_)))
    );
    assert!(
        parsed
            .calls
            .iter()
            .any(|call| matches!(call.target, ParsedTarget::Local(_)))
    );
    assert!(parsed.calls.iter().any(
        |call| matches!(&call.target, ParsedTarget::Unresolved(name) if name == "self.method")
    ));

    for kind in [
        EntryPointKind::Executable,
        EntryPointKind::Test,
        EntryPointKind::Benchmark,
    ] {
        assert!(
            parsed.entry_points.iter().any(|entry| entry.kind == kind),
            "missing {kind:?} entry point"
        );
    }
}

#[test]
fn spans_use_one_based_lines_and_utf8_byte_columns() {
    let parsed = parse_fixture("src/unicode.rs", include_str!("fixtures/unicode.rs"));
    let unicode_call = parsed
        .calls
        .iter()
        .find(|call| {
            matches!(
                call.target,
                ParsedTarget::Local(local_id)
                    if parsed.symbols.iter().any(|symbol| {
                        symbol.local_id == local_id && symbol.name == "café"
                    })
            )
        })
        .expect("unicode call should be extracted");

    assert_eq!(unicode_call.span.start().line(), 3);
    assert_eq!(unicode_call.span.start().column(), 13);
    assert_eq!(unicode_call.span.end().line(), 3);
    assert_eq!(unicode_call.span.end().column(), 20);
}

#[test]
fn syntax_errors_are_reported() {
    let path = RepositoryPath::new("src/broken.rs").expect("canonical path");
    let error = RustParser::new()
        .parse(ParseInput {
            path: &path,
            source: "fn broken(",
        })
        .expect_err("invalid Rust should fail");
    assert_eq!(error.path, path);
    assert!(error.message.contains("syntax errors"));
}
