use std::{
    fmt::Write as _,
    fs,
    future::Future,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use codeatlas_core::{
    CallEdge, CallEdgeId, EntryPoint, EntryPointId, EntryPointKind, FileId, FileInfo, Language,
    Module, ModuleId, ReferenceEdge, ReferenceEdgeId, ReferenceKind, RepositoryId, RepositoryModel,
    RepositoryPath, SourceSpan, Symbol, SymbolId, SymbolKind, TargetResolution, ToolCall,
    ToolCallId, ToolError, ToolExecutor, UnresolvedTarget,
};
use codeatlas_query::{
    FindReferencesQuery, FindSymbolQuery, GetModuleQuery, GetRepositoryOverviewQuery,
    GetSymbolQuery, ListFilesQuery, QueryError, ReadFileQuery, RepositoryIndex, RepositoryTools,
    ResolutionMethod, SearchCodeQuery, TOOL_NAMES, TraceCallQuery, TraceDirection,
};
use serde_json::{Value, json};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

const SOURCE_A: &str = "fn alpha() {\n    beta();\n    duplicate();\n    external();\n}\n";
const SOURCE_B: &str =
    "fn beta() {\n    alpha();\n    local();\n}\nfn duplicate() {}\nfn local() {}\n";
const SOURCE_C: &str = "fn duplicate() {}\nfn local() {}\n";

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codeatlas-query-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("temporary repository should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, content: &[u8]) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("source parent should be created");
        }
        fs::write(path, content).expect("source should be written");
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Clone, Copy)]
struct FixtureIds {
    alpha: SymbolId,
    module_b: ModuleId,
}

struct Fixture {
    root: TestDirectory,
    model: RepositoryModel,
    ids: FixtureIds,
}

impl Fixture {
    #[allow(clippy::too_many_lines)]
    fn new() -> Self {
        let root = TestDirectory::new("fixture");
        root.write("src/a.rs", SOURCE_A.as_bytes());
        root.write("src/b.rs", SOURCE_B.as_bytes());
        root.write("src/c.rs", SOURCE_C.as_bytes());

        let path_a = repository_path("src/a.rs");
        let path_b = repository_path("src/b.rs");
        let path_c = repository_path("src/c.rs");
        let file_a = file_id(&path_a);
        let file_b = file_id(&path_b);
        let file_c = file_id(&path_c);
        let module_a = module_id("crate::a");
        let module_b = module_id("crate::b");
        let module_c = module_id("crate::c");
        let alpha = symbol_id("crate::a::alpha");
        let beta = symbol_id("crate::b::beta");
        let duplicate_b = symbol_id("crate::b::duplicate");
        let duplicate_c = symbol_id("crate::c::duplicate");
        let local_b = symbol_id("crate::b::local");
        let local_c = symbol_id("crate::c::local");

        let mut model = RepositoryModel::empty(
            RepositoryId::from_stable_parts(&["query-fixture"]),
            "query-fixture",
        );
        model.files = vec![
            file(file_c, path_c, Language::Rust),
            file(file_a, path_a, Language::Rust),
            file(file_b, path_b, Language::Rust),
        ];
        model.modules = vec![
            module(module_c, "c", "crate::c", file_c, span(1, 0, 2, 13)),
            module(module_a, "a", "crate::a", file_a, span(1, 0, 5, 1)),
            module(module_b, "b", "crate::b", file_b, span(1, 0, 6, 13)),
        ];
        model.symbols = vec![
            symbol(
                duplicate_c,
                "duplicate",
                "crate::c::duplicate",
                file_c,
                module_c,
                span(1, 0, 1, 17),
            ),
            symbol(
                alpha,
                "alpha",
                "crate::a::alpha",
                file_a,
                module_a,
                span(1, 0, 5, 1),
            ),
            symbol(
                local_b,
                "local",
                "crate::b::local",
                file_b,
                module_b,
                span(6, 0, 6, 13),
            ),
            symbol(
                beta,
                "beta",
                "crate::b::beta",
                file_b,
                module_b,
                span(1, 0, 4, 1),
            ),
            symbol(
                duplicate_b,
                "duplicate",
                "crate::b::duplicate",
                file_b,
                module_b,
                span(5, 0, 5, 17),
            ),
            symbol(
                local_c,
                "local",
                "crate::c::local",
                file_c,
                module_c,
                span(2, 0, 2, 13),
            ),
        ];
        model.references = vec![
            reference(
                "ambiguous-duplicate",
                file_a,
                Some(alpha),
                "duplicate",
                span(3, 4, 3, 13),
            ),
            reference("alpha-use", file_b, Some(beta), "alpha", span(2, 4, 2, 9)),
        ];
        model.calls = vec![
            call("beta-local", file_b, Some(beta), "local", span(3, 4, 3, 9)),
            call(
                "alpha-external",
                file_a,
                Some(alpha),
                "external",
                span(4, 4, 4, 12),
            ),
            call(
                "beta-alpha",
                file_b,
                Some(beta),
                "crate::a::alpha",
                span(2, 4, 2, 9),
            ),
            call(
                "alpha-duplicate",
                file_a,
                Some(alpha),
                "duplicate",
                span(3, 4, 3, 13),
            ),
            call("alpha-beta", file_a, Some(alpha), "beta", span(2, 4, 2, 8)),
        ];
        model.entry_points.push(EntryPoint {
            id: EntryPointId::from_stable_parts(&["alpha-entry"]),
            kind: EntryPointKind::Executable,
            label: "alpha".to_owned(),
            file_id: file_a,
            symbol_id: Some(alpha),
            span: span(1, 0, 5, 1),
        });

        Self {
            root,
            model,
            ids: FixtureIds { alpha, module_b },
        }
    }

    fn index(&self) -> RepositoryIndex {
        RepositoryIndex::new(self.root.path(), self.model.clone())
            .expect("fixture model should index")
    }
}

fn repository_path(value: &str) -> RepositoryPath {
    RepositoryPath::new(value).expect("fixture path should be canonical")
}

fn span(start_line: u32, start_column: u32, end_line: u32, end_column: u32) -> SourceSpan {
    SourceSpan::new(start_line, start_column, end_line, end_column)
        .expect("fixture span should be valid")
}

fn file_id(path: &RepositoryPath) -> FileId {
    FileId::from_stable_parts(&[path.as_str()])
}

fn module_id(qualified_name: &str) -> ModuleId {
    ModuleId::from_stable_parts(&[qualified_name])
}

fn symbol_id(qualified_name: &str) -> SymbolId {
    SymbolId::from_stable_parts(&[qualified_name])
}

fn file(id: FileId, path: RepositoryPath, language: Language) -> FileInfo {
    FileInfo { id, path, language }
}

fn module(
    id: ModuleId,
    name: &str,
    qualified_name: &str,
    file_id: FileId,
    source_span: SourceSpan,
) -> Module {
    Module {
        id,
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        file_id,
        span: Some(source_span),
        parent_id: None,
    }
}

fn symbol(
    id: SymbolId,
    name: &str,
    qualified_name: &str,
    file_id: FileId,
    module_id: ModuleId,
    source_span: SourceSpan,
) -> Symbol {
    Symbol {
        id,
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind: SymbolKind::Function,
        file_id,
        module_id: Some(module_id),
        span: source_span,
        parent_id: None,
    }
}

fn symbol_without_module(
    id: SymbolId,
    name: &str,
    qualified_name: &str,
    file_id: FileId,
) -> Symbol {
    Symbol {
        id,
        name: name.to_owned(),
        qualified_name: qualified_name.to_owned(),
        kind: SymbolKind::Function,
        file_id,
        module_id: None,
        span: span(1, 0, 1, 6),
        parent_id: None,
    }
}

fn unresolved(name: &str) -> TargetResolution<SymbolId> {
    TargetResolution::Unresolved(UnresolvedTarget {
        name: name.to_owned(),
        reason: Some("parser left target unresolved".to_owned()),
    })
}

fn call(
    key: &str,
    source_file_id: FileId,
    caller_id: Option<SymbolId>,
    target: &str,
    source_span: SourceSpan,
) -> CallEdge {
    CallEdge {
        id: CallEdgeId::from_stable_parts(&[key]),
        source_file_id,
        caller_id,
        target: unresolved(target),
        span: source_span,
        confidence: None,
    }
}

fn reference(
    key: &str,
    source_file_id: FileId,
    source_symbol_id: Option<SymbolId>,
    target: &str,
    source_span: SourceSpan,
) -> ReferenceEdge {
    ReferenceEdge {
        id: ReferenceEdgeId::from_stable_parts(&[key]),
        source_file_id,
        source_symbol_id,
        target: unresolved(target),
        kind: ReferenceKind::Read,
        span: source_span,
    }
}

#[test]
fn typed_queries_cover_all_nine_repository_operations() {
    let fixture = Fixture::new();
    let index = fixture.index();

    let files = index
        .list_files(&ListFilesQuery::default())
        .expect("files should list");
    let paths = files
        .data
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["src/a.rs", "src/b.rs", "src/c.rs"]);

    let read = index
        .read_file(&ReadFileQuery {
            path: repository_path("src/a.rs"),
            start_line: Some(2),
            end_line: Some(3),
        })
        .expect("indexed source should read");
    assert_eq!(read.data.content, "    beta();\n    duplicate();\n");
    assert_eq!(read.evidence.len(), 1);

    let symbols = index
        .find_symbol(&FindSymbolQuery {
            query: "alpha".to_owned(),
            case_sensitive: None,
            limit: None,
        })
        .expect("symbol should be found");
    assert_eq!(symbols.data.symbols[0].id, fixture.ids.alpha);

    let references = index
        .find_references(&FindReferencesQuery {
            symbol_id: fixture.ids.alpha,
            limit: None,
        })
        .expect("references should resolve");
    assert_eq!(references.data.total, 1);

    let symbol = index
        .get_symbol(&GetSymbolQuery {
            symbol_id: fixture.ids.alpha,
        })
        .expect("symbol detail should resolve");
    assert_eq!(symbol.data.callers_total, 1);
    assert_eq!(symbol.data.callees_total, 3);
    assert_eq!(symbol.data.entry_points.len(), 1);

    let module = index
        .get_module(&GetModuleQuery {
            module_id: Some(fixture.ids.module_b),
            qualified_name: None,
            limit: None,
        })
        .expect("module should resolve");
    assert_eq!(module.data.module.qualified_name, "crate::b");
    assert_eq!(module.data.symbol_total, 3);

    let search = index
        .search_code(&SearchCodeQuery {
            query: "alpha".to_owned(),
            path_prefix: None,
            case_sensitive: Some(true),
            limit: Some(10),
        })
        .expect("source search should succeed");
    assert_eq!(search.data.matches.len(), 2);
    assert_eq!(search.data.matches[0].path.as_str(), "src/a.rs");

    let trace = index
        .trace_call(&TraceCallQuery {
            symbol_id: fixture.ids.alpha,
            direction: TraceDirection::Callees,
            max_depth: Some(3),
            max_nodes: Some(10),
        })
        .expect("call trace should succeed");
    assert!(trace.data.edges.iter().any(|edge| edge.revisited));

    let overview = index
        .get_repository_overview(&GetRepositoryOverviewQuery::default())
        .expect("overview should succeed");
    assert_eq!(overview.data.counts.files, 3);
    assert_eq!(overview.data.counts.symbols, 6);
    assert_eq!(overview.data.counts.unresolved_calls, 2);
}

#[test]
fn repository_overview_is_compact_and_omits_tests_by_default() {
    let fixture = Fixture::new();
    let mut model = fixture.model.clone();
    let test_file = model.files[0].id;
    model.entry_points.push(EntryPoint {
        id: EntryPointId::from_stable_parts(&["test-entry"]),
        kind: EntryPointKind::Test,
        label: "test_alpha".to_owned(),
        file_id: test_file,
        symbol_id: None,
        span: span(1, 0, 2, 0),
    });
    let index = RepositoryIndex::new(fixture.root.path(), model).expect("valid fixture index");

    let compact = index
        .get_repository_overview(&GetRepositoryOverviewQuery {
            limit: Some(1),
            include_tests: None,
        })
        .expect("compact overview");
    assert_eq!(compact.data.modules.len(), 1);
    assert!(compact.data.modules_truncated);
    assert_eq!(compact.data.entry_points.len(), 1);
    assert_eq!(
        compact.data.entry_points[0].kind,
        EntryPointKind::Executable
    );
    assert!(compact.data.entry_points_truncated);
    assert_eq!(compact.evidence.len(), 1);
    assert!(
        compact.evidence[0]
            .excerpt
            .as_ref()
            .is_none_or(|excerpt| excerpt.len() <= 512)
    );

    let with_tests = index
        .get_repository_overview(&GetRepositoryOverviewQuery {
            limit: Some(10),
            include_tests: Some(true),
        })
        .expect("overview with tests");
    assert!(
        with_tests
            .data
            .entry_points
            .iter()
            .any(|entry| entry.kind == EntryPointKind::Test)
    );

    assert!(matches!(
        index.get_repository_overview(&GetRepositoryOverviewQuery {
            limit: Some(51),
            include_tests: None,
        }),
        Err(QueryError::InvalidQuery { .. })
    ));
}

#[test]
fn conservative_resolution_never_selects_an_ambiguous_simple_name() {
    let fixture = Fixture::new();
    let index = fixture.index();

    let ambiguous = index
        .resolved_calls()
        .iter()
        .find(|call| {
            matches!(
                &call.target.original,
                TargetResolution::Unresolved(target) if target.name == "duplicate"
            )
        })
        .expect("ambiguous call should exist");
    assert!(matches!(
        ambiguous.target.resolved,
        TargetResolution::Unresolved(_)
    ));
    assert_eq!(ambiguous.target.method, ResolutionMethod::Unresolved);

    let exact = index
        .resolved_calls()
        .iter()
        .find(|call| {
            matches!(
                &call.target.original,
                TargetResolution::Unresolved(target) if target.name == "crate::a::alpha"
            )
        })
        .expect("qualified call should exist");
    assert_eq!(exact.target.method, ResolutionMethod::ExactQualifiedName);
    assert!(matches!(
        exact.target.resolved,
        TargetResolution::Resolved(id) if id == fixture.ids.alpha
    ));

    let same_module = index
        .resolved_calls()
        .iter()
        .find(|call| {
            matches!(
                &call.target.original,
                TargetResolution::Unresolved(target) if target.name == "local"
            )
        })
        .expect("same-module call should exist");
    assert_eq!(same_module.target.method, ResolutionMethod::SameModule);
    assert!(
        index
            .model()
            .calls
            .iter()
            .all(|call| matches!(call.target, TargetResolution::Unresolved(_)))
    );
}

#[test]
fn conservative_resolution_uses_a_unique_same_file_candidate() {
    let root = TestDirectory::new("same-file");
    let source_path = repository_path("source.rs");
    let other_path = repository_path("other.rs");
    let source_file = file_id(&source_path);
    let other_file = file_id(&other_path);
    let caller = symbol_id("caller");
    let local_target = symbol_id("source::target");
    let other_target = symbol_id("other::target");
    let mut model =
        RepositoryModel::empty(RepositoryId::from_stable_parts(&["same-file"]), "same-file");
    model.files = vec![
        file(source_file, source_path, Language::Rust),
        file(other_file, other_path, Language::Rust),
    ];
    model.symbols = vec![
        symbol_without_module(caller, "caller", "caller", source_file),
        symbol_without_module(local_target, "target", "source::target", source_file),
        symbol_without_module(other_target, "target", "other::target", other_file),
    ];
    model.calls.push(call(
        "same-file-target",
        source_file,
        Some(caller),
        "target",
        span(1, 0, 1, 6),
    ));

    let index = RepositoryIndex::new(root.path(), model).expect("same-file model should index");
    let target = &index.resolved_calls()[0].target;
    assert_eq!(target.method, ResolutionMethod::SameFile);
    assert!(matches!(
        target.resolved,
        TargetResolution::Resolved(id) if id == local_target
    ));
}

#[test]
fn trace_handles_cycles_unresolved_edges_and_limits() {
    let fixture = Fixture::new();
    let index = fixture.index();

    let full = index
        .trace_call(&TraceCallQuery {
            symbol_id: fixture.ids.alpha,
            direction: TraceDirection::Callees,
            max_depth: Some(4),
            max_nodes: Some(10),
        })
        .expect("cycle-safe trace should succeed");
    assert_eq!(full.data.nodes.len(), 3);
    assert!(full.data.edges.iter().any(|edge| edge.revisited));
    assert!(full.data.edges.iter().any(|edge| {
        matches!(edge.call.target.resolved, TargetResolution::Unresolved(_))
            && edge.next_symbol_id.is_none()
    }));

    let shallow = index
        .trace_call(&TraceCallQuery {
            symbol_id: fixture.ids.alpha,
            direction: TraceDirection::Callees,
            max_depth: Some(1),
            max_nodes: Some(10),
        })
        .expect("depth-limited trace should succeed");
    assert_eq!(shallow.data.nodes.len(), 2);
    assert!(shallow.data.truncated_by_depth);

    let one_node = index
        .trace_call(&TraceCallQuery {
            symbol_id: fixture.ids.alpha,
            direction: TraceDirection::Callees,
            max_depth: Some(4),
            max_nodes: Some(1),
        })
        .expect("node-limited trace should succeed");
    assert_eq!(one_node.data.nodes.len(), 1);
    assert!(one_node.data.truncated_by_node_limit);
    assert!(
        one_node
            .data
            .edges
            .iter()
            .any(|edge| edge.omitted_by_node_limit)
    );

    let callers = index
        .trace_call(&TraceCallQuery {
            symbol_id: fixture.ids.alpha,
            direction: TraceDirection::Callers,
            max_depth: Some(4),
            max_nodes: Some(10),
        })
        .expect("caller trace should succeed");
    assert_eq!(callers.data.nodes.len(), 2);
    assert!(callers.data.edges.iter().any(|edge| edge.revisited));
}

#[test]
fn evidence_has_stable_ids_exact_spans_and_bounded_excerpts() {
    let fixture = Fixture::new();
    let index = fixture.index();
    let query = FindSymbolQuery {
        query: "alpha".to_owned(),
        case_sensitive: Some(true),
        limit: None,
    };
    let first = index
        .find_symbol(&query)
        .expect("symbol evidence should build");
    let second = index
        .find_symbol(&query)
        .expect("symbol evidence should repeat");
    let evidence = &first.evidence[0];

    assert_eq!(evidence.id, second.evidence[0].id);
    assert_eq!(evidence.path.as_str(), "src/a.rs");
    assert_eq!(evidence.span, span(1, 0, 5, 1));
    assert_eq!(evidence.symbol_id, Some(fixture.ids.alpha));
    let excerpt = evidence
        .excerpt
        .as_deref()
        .expect("excerpt should be available");
    assert!(excerpt.contains("fn alpha()"));
    assert!(excerpt.len() <= 2 * 1024);

    let search = index
        .search_code(&SearchCodeQuery {
            query: "alpha".to_owned(),
            path_prefix: Some(repository_path("src/a.rs")),
            case_sensitive: Some(true),
            limit: Some(1),
        })
        .expect("search evidence should build");
    assert_eq!(search.evidence[0].span, span(1, 3, 1, 8));
    assert_eq!(search.evidence[0].excerpt.as_deref(), Some("fn alpha() {"));
}

#[test]
fn index_and_query_order_are_stable_for_shuffled_models() {
    let fixture = Fixture::new();
    let first = fixture.index();
    let mut shuffled = fixture.model.clone();
    shuffled.files.reverse();
    shuffled.modules.reverse();
    shuffled.symbols.reverse();
    shuffled.references.reverse();
    shuffled.calls.reverse();
    shuffled.entry_points.reverse();
    let second =
        RepositoryIndex::new(fixture.root.path(), shuffled).expect("shuffled model should index");

    let file_query = ListFilesQuery::default();
    assert_eq!(
        as_json(&first.list_files(&file_query).expect("first list")),
        as_json(&second.list_files(&file_query).expect("second list"))
    );
    let trace_query = TraceCallQuery {
        symbol_id: fixture.ids.alpha,
        direction: TraceDirection::Callees,
        max_depth: Some(4),
        max_nodes: Some(10),
    };
    assert_eq!(
        as_json(&first.trace_call(&trace_query).expect("first trace")),
        as_json(&second.trace_call(&trace_query).expect("second trace"))
    );
}

#[test]
fn all_tool_calls_return_the_json_envelope_and_definitions_are_strict() {
    let fixture = Fixture::new();
    let ids = fixture.ids;
    let tools = RepositoryTools::new(fixture.index());
    let definitions = RepositoryTools::definitions();
    assert_eq!(definitions.len(), TOOL_NAMES.len());
    for (definition, expected_name) in definitions.iter().zip(TOOL_NAMES) {
        assert_eq!(definition.name, expected_name);
        assert_eq!(definition.input_schema["additionalProperties"], false);
        let output = definition
            .output_schema
            .as_ref()
            .expect("output schema should be declared");
        assert_eq!(output["required"], json!(["data", "evidence"]));
    }

    let calls = [
        ("list_files", json!({})),
        ("read_file", json!({"path": "src/a.rs"})),
        ("find_symbol", json!({"query": "alpha"})),
        ("find_references", json!({"symbol_id": ids.alpha})),
        ("get_symbol", json!({"symbol_id": ids.alpha})),
        ("get_module", json!({"qualified_name": "crate::b"})),
        ("search_code", json!({"query": "alpha"})),
        (
            "trace_call",
            json!({"symbol_id": ids.alpha, "direction": "callees"}),
        ),
        ("get_repository_overview", json!({})),
    ];
    for (name, arguments) in calls {
        let call = tool_call(name, arguments);
        let output = block_on(tools.execute(&call)).expect("valid tool should execute");
        assert!(!output.is_error);
        let object = output
            .result
            .as_object()
            .expect("result should be an object");
        assert_eq!(object.len(), 2);
        assert!(object.contains_key("data"));
        assert!(object["evidence"].is_array());
    }
}

#[test]
fn invalid_json_types_unknown_fields_and_unknown_tools_are_tool_errors() {
    let fixture = Fixture::new();
    let tools = RepositoryTools::new(fixture.index());
    let invalid_calls = [
        tool_call("list_files", json!({"unexpected": true})),
        tool_call("read_file", json!({"path": 42})),
        tool_call(
            "trace_call",
            json!({
                "symbol_id": fixture.ids.alpha,
                "direction": "callees",
                "max_depth": "many"
            }),
        ),
        tool_call("read_file", json!({"path": "../secret.rs"})),
    ];
    for call in invalid_calls {
        let error = block_on(tools.execute(&call)).expect_err("arguments should be rejected");
        assert!(matches!(error, ToolError::InvalidArguments { .. }));
    }

    let unknown = tool_call("run_shell", json!({}));
    let error = block_on(tools.execute(&unknown)).expect_err("unknown tool should fail");
    assert!(matches!(error, ToolError::NotFound { .. }));
}

#[test]
fn tool_execution_yields_to_a_current_thread_runtime() {
    let fixture = Fixture::new();
    let tools = RepositoryTools::new(fixture.index());
    let call = tool_call("list_files", json!({}));
    let expected_call_id = call.id;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .build()
        .expect("current-thread runtime should build");

    runtime.block_on(async move {
        let (blocker_started_tx, blocker_started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let blocker = tokio::task::spawn_blocking(move || {
            blocker_started_tx
                .send(())
                .expect("test should observe the occupied blocking thread");
            release_rx
                .recv()
                .expect("test should release the blocking thread");
        });
        blocker_started_rx
            .await
            .expect("blocking thread should start");

        let (execution_started_tx, execution_started_rx) = tokio::sync::oneshot::channel();
        let execution = tokio::spawn(async move {
            execution_started_tx
                .send(())
                .expect("test should observe the tool task");
            tools.execute(&call).await
        });
        execution_started_rx.await.expect("tool task should start");
        let yielded_while_blocking_pool_was_busy = !execution.is_finished();

        release_tx
            .send(())
            .expect("blocking thread should still be waiting");
        blocker.await.expect("blocking task should finish");
        let output = execution
            .await
            .expect("tool task should finish")
            .expect("tool execution should succeed");

        assert!(
            yielded_while_blocking_pool_was_busy,
            "tool dispatch should wait on the blocking pool without blocking the runtime"
        );
        assert_eq!(output.call_id, expected_call_id);
    });
}

#[test]
fn local_source_ranges_are_complete_while_model_reads_are_paginated() {
    let root = TestDirectory::new("complete-source-range");
    let mut source = String::new();
    let padding = "x".repeat(96);
    for line in 1..=700 {
        writeln!(source, "// source line {line:04} {padding}")
            .expect("writing to a String should succeed");
    }
    assert!(source.len() > 64 * 1024);
    root.write("long.rs", source.as_bytes());
    let path = repository_path("long.rs");
    let mut model = RepositoryModel::empty(
        RepositoryId::from_stable_parts(&["complete-source-range"]),
        "complete-source-range",
    );
    model
        .files
        .push(file(file_id(&path), path.clone(), Language::Rust));
    let index = RepositoryIndex::new(root.path(), model).expect("source index should build");
    let query = ReadFileQuery {
        path,
        start_line: Some(1),
        end_line: Some(700),
    };

    let model_read = index
        .read_file(&query)
        .expect("oversized model range should paginate instead of failing");
    assert!(model_read.data.truncated);
    assert!(model_read.data.end_line <= 500);
    assert!(model_read.data.content.len() <= 64 * 1024);

    let local_read = index
        .read_source_range(&query)
        .expect("local viewer should receive the complete requested range");
    assert_eq!(local_read.start_line, 1);
    assert_eq!(local_read.end_line, 700);
    assert_eq!(local_read.content, source);
    assert!(!local_read.truncated);
}

#[cfg(unix)]
#[test]
fn reads_reject_unindexed_paths_symlink_escape_and_binary_files() {
    use std::os::unix::fs::symlink;

    let root = TestDirectory::new("safe-root");
    let outside = TestDirectory::new("outside-root");
    root.write("ok.rs", b"fn ok() {}\n");
    root.write("binary.dat", b"text\0binary");
    outside.write("secret.rs", b"secret\n");
    symlink(
        outside.path().join("secret.rs"),
        root.path().join("escape.rs"),
    )
    .expect("test symlink should be created");

    let mut model =
        RepositoryModel::empty(RepositoryId::from_stable_parts(&["safe-root"]), "safe-root");
    for path in ["escape.rs", "binary.dat", "ok.rs"] {
        let path = repository_path(path);
        model.files.push(file(file_id(&path), path, Language::Rust));
    }
    let index = RepositoryIndex::new(root.path(), model).expect("security model should index");

    let unindexed = index.read_file(&ReadFileQuery {
        path: repository_path("secret.rs"),
        start_line: None,
        end_line: None,
    });
    assert!(matches!(unindexed, Err(QueryError::NotFound { .. })));

    let escaped = index.read_file(&ReadFileQuery {
        path: repository_path("escape.rs"),
        start_line: None,
        end_line: None,
    });
    assert!(matches!(escaped, Err(QueryError::PathEscape { .. })));

    let binary = index.read_file(&ReadFileQuery {
        path: repository_path("binary.dat"),
        start_line: None,
        end_line: None,
    });
    assert!(matches!(binary, Err(QueryError::BinaryFile { .. })));

    let escaped_search = index.search_code(&SearchCodeQuery {
        query: "secret".to_owned(),
        path_prefix: Some(repository_path("escape.rs")),
        case_sensitive: None,
        limit: None,
    });
    assert!(matches!(escaped_search, Err(QueryError::PathEscape { .. })));
}

fn tool_call(name: &str, arguments: Value) -> ToolCall {
    ToolCall {
        id: ToolCallId::from_stable_parts(&[name, &arguments.to_string()]),
        name: name.to_owned(),
        arguments,
    }
}

fn as_json(value: &impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("query result should serialize")
}

fn block_on<F: Future>(future: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("current-thread runtime should build")
        .block_on(future)
}
