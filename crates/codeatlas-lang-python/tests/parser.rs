use codeatlas_core::{
    EntryPointKind, Language, ParseInput, ParsedFile, ParsedTarget, ParserAdapter, ReferenceKind,
    RepositoryPath, SymbolKind,
};
use codeatlas_lang_python::PythonParser;

fn parse(path: &str, source: &str) -> ParsedFile {
    let path = RepositoryPath::new(path).expect("test path should be valid");
    PythonParser::new()
        .parse(ParseInput {
            path: &path,
            source,
        })
        .expect("Python source should produce a parsed file")
}

#[test]
fn reports_language_path_module_and_nested_symbols() {
    let source = r#"MAX_RETRIES = 3

def outer():
    async def fetch():
        return None
    return fetch()

class Worker:
    KIND = "worker"

    def run(self):
        def inner():
            return 1
        return inner()
"#;
    let parsed = parse("pkg/jobs/worker.py", source);

    assert_eq!(parsed.language, Language::Python);
    assert_eq!(parsed.path.as_str(), "pkg/jobs/worker.py");
    let module = parsed.module.as_ref().expect("module should be present");
    assert_eq!(module.name, "worker");
    assert_eq!(module.qualified_name, "pkg.jobs.worker");

    let symbols: Vec<_> = parsed
        .symbols
        .iter()
        .map(|symbol| {
            (
                symbol.name.as_str(),
                symbol.qualified_name.as_str(),
                &symbol.kind,
                symbol.parent_id,
            )
        })
        .collect();
    assert_eq!(symbols[0].0, "MAX_RETRIES");
    assert_eq!(symbols[0].2, &SymbolKind::Constant);
    assert_eq!(symbols[1].1, "pkg.jobs.worker.outer");
    assert_eq!(symbols[1].2, &SymbolKind::Function);
    assert_eq!(symbols[2].1, "pkg.jobs.worker.outer.fetch");
    assert_eq!(symbols[2].2, &SymbolKind::Function);
    assert_eq!(symbols[2].3, Some(parsed.symbols[1].local_id));
    assert_eq!(symbols[3].1, "pkg.jobs.worker.Worker");
    assert_eq!(symbols[3].2, &SymbolKind::Class);
    assert_eq!(symbols[4].1, "pkg.jobs.worker.Worker.KIND");
    assert_eq!(symbols[4].3, Some(parsed.symbols[3].local_id));
    assert_eq!(symbols[5].1, "pkg.jobs.worker.Worker.run");
    assert_eq!(symbols[5].2, &SymbolKind::Method);
    assert_eq!(symbols[5].3, Some(parsed.symbols[3].local_id));
    assert_eq!(symbols[6].1, "pkg.jobs.worker.Worker.run.inner");
    assert_eq!(symbols[6].2, &SymbolKind::Function);
    assert_eq!(symbols[6].3, Some(parsed.symbols[5].local_id));
    assert_eq!(parsed, parse("pkg/jobs/worker.py", source));
}

#[test]
fn derives_package_module_name_for_init_files() {
    let parsed = parse("pkg/subpkg/__init__.py", "VALUE = 1\n");
    let module = parsed.module.expect("module should be present");
    assert_eq!(module.name, "subpkg");
    assert_eq!(module.qualified_name, "pkg.subpkg");
}

#[test]
fn supports_and_parses_python_stub_paths_case_insensitively() {
    for path in ["module.PY", "module.pyi", "module.PYI"] {
        let path = RepositoryPath::new(path).expect("test path should be valid");
        assert!(PythonParser::supports_path(&path));
    }

    let parsed = parse(
        "pkg/contracts.PyI",
        "class UserRecord:\n    id: int\n\ndef load() -> UserRecord: ...\n",
    );
    let module = parsed.module.expect("stub module should be present");
    assert_eq!(module.name, "contracts");
    assert_eq!(module.qualified_name, "pkg.contracts");
    assert!(parsed.symbols.iter().any(|symbol| {
        symbol.name == "UserRecord"
            && symbol.qualified_name == "pkg.contracts.UserRecord"
            && symbol.kind == SymbolKind::Class
    }));
    assert!(parsed.symbols.iter().any(|symbol| {
        symbol.name == "load"
            && symbol.qualified_name == "pkg.contracts.load"
            && symbol.kind == SymbolKind::Function
    }));
}

#[test]
fn extracts_import_aliases_and_relative_targets() {
    let source = r"import os
import package.tools as tools, json as js
from .models import User, Group as Team
from .. import settings
from package.api import *
";
    let parsed = parse("pkg/services.py", source);
    let imports: Vec<_> = parsed
        .imports
        .iter()
        .map(|import| (import.target.as_str(), import.alias.as_deref()))
        .collect();

    assert_eq!(
        imports,
        vec![
            ("os", None),
            ("package.tools", Some("tools")),
            ("json", Some("js")),
            (".models.User", None),
            (".models.Group", Some("Team")),
            ("..settings", None),
            ("package.api.*", None),
        ]
    );
}

#[test]
fn resolves_bare_local_calls_and_preserves_attribute_calls() {
    let source = r"def helper():
    pass

def run(service):
    helper()
    service.run()
";
    let parsed = parse("app.py", source);
    let helper = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.name == "helper")
        .expect("helper should be extracted");
    let run = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.name == "run")
        .expect("run should be extracted");

    assert_eq!(parsed.calls.len(), 2);
    assert_eq!(parsed.calls[0].caller_id, Some(run.local_id));
    assert_eq!(parsed.calls[0].target, ParsedTarget::Local(helper.local_id));
    assert_eq!(parsed.calls[1].caller_id, Some(run.local_id));
    assert_eq!(
        parsed.calls[1].target,
        ParsedTarget::Unresolved("service.run".to_owned())
    );
}

#[test]
fn extracts_inheritance_and_type_references_without_definition_names() {
    let source = r"class Base:
    pass

class Child(Base):
    value: models.Value

    def convert(self, item: Base) -> list[Base]:
        return item
";
    let parsed = parse("types.py", source);
    let base = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Base")
        .expect("Base should be extracted");
    let child = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.name == "Child")
        .expect("Child should be extracted");

    let inheritance = parsed
        .references
        .iter()
        .find(|reference| reference.kind == ReferenceKind::Inheritance)
        .expect("inheritance reference should be extracted");
    assert_eq!(inheritance.source_id, Some(child.local_id));
    assert_eq!(inheritance.target, ParsedTarget::Local(base.local_id));
    assert!(
        parsed
            .references
            .iter()
            .any(|reference| reference.kind == ReferenceKind::Type
                && reference.target == ParsedTarget::Unresolved("models.Value".to_owned()))
    );
    assert!(!parsed.references.iter().any(|reference| {
        reference.span == base.span && reference.target == ParsedTarget::Local(base.local_id)
    }));
}

#[test]
fn recognizes_main_guard_and_common_test_entry_points() {
    let source = r#"import unittest

def test_top_level():
    pass

class TestPytestStyle:
    def test_method(self):
        pass

class LegacySuite(unittest.TestCase):
    def test_legacy(self):
        pass

if __name__ == "__main__":
    unittest.main()
"#;
    let parsed = parse("tests/test_sample.py", source);

    assert!(
        parsed
            .entry_points
            .iter()
            .any(|entry| { entry.kind == EntryPointKind::Executable && entry.symbol_id.is_none() })
    );
    let test_labels: Vec<_> = parsed
        .entry_points
        .iter()
        .filter(|entry| entry.kind == EntryPointKind::Test)
        .map(|entry| entry.label.as_str())
        .collect();
    assert_eq!(
        test_labels,
        vec![
            "tests.test_sample.test_top_level",
            "tests.test_sample.TestPytestStyle",
            "tests.test_sample.TestPytestStyle.test_method",
            "tests.test_sample.LegacySuite",
            "tests.test_sample.LegacySuite.test_legacy",
        ]
    );
}

#[test]
fn spans_use_one_based_lines_and_utf8_byte_columns() {
    let source = "def café():\n    message = '你好'; helper()\n";
    let parsed = parse("unicode.py", source);
    let function = parsed
        .symbols
        .iter()
        .find(|symbol| symbol.name == "café")
        .expect("Unicode function should be extracted");
    let call = parsed.calls.first().expect("call should be extracted");

    assert_eq!(function.span.start().line(), 1);
    assert_eq!(function.span.start().column(), 0);
    assert_eq!(function.span.end().line(), 2);
    assert_eq!(call.span.start().line(), 2);
    assert_eq!(call.span.start().column(), 24);
    assert_eq!(call.span.end().column(), 32);
}

#[test]
fn keeps_structures_around_local_syntax_errors() {
    let source = r"def before():
    return 1

broken = (

def after():
    return before()
";
    let parsed = parse("partial.py", source);

    assert!(parsed.symbols.iter().any(|symbol| symbol.name == "before"));
    assert!(parsed.symbols.iter().any(|symbol| symbol.name == "after"));
    assert!(
        parsed
            .calls
            .iter()
            .any(|call| matches!(call.target, ParsedTarget::Local(_)))
    );
}

#[test]
fn parsed_file_round_trips_through_serde() {
    let parsed = parse(
        "roundtrip.py",
        "from .dependency import value as imported\n\ndef run():\n    value()\n",
    );
    let encoded = serde_json::to_string(&parsed).expect("parsed file should serialize");
    let decoded: ParsedFile =
        serde_json::from_str(&encoded).expect("parsed file should deserialize");
    assert_eq!(decoded, parsed);
}

#[test]
fn rejects_non_python_paths() {
    let path = RepositoryPath::new("module.txt").expect("test path should be valid");
    assert!(!PythonParser::supports_path(&path));
    let error = PythonParser::new()
        .parse(ParseInput {
            path: &path,
            source: "def run(): pass\n",
        })
        .expect_err("unsupported paths should be rejected");
    assert_eq!(error.path, path);
    assert_eq!(error.message, "expected a .py or .pyi repository path");
}
