use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
};

use codeatlas_core::{
    Language, ParseInput, ParsedCall, ParsedFile, ParsedSymbol, ParsedSymbolId, ParsedTarget,
    ParserAdapter, ParserError, ProgressPhase, RepositoryPath, SchemaVersion, SourceSpan,
    SymbolKind, TargetResolution,
};
use codeatlas_indexer::{
    CachedIndex, IndexAssembler, IndexError, IndexStoreError, Indexer, JsonIndexStore,
    ParserRegistry, RepositoryScanner, RepositorySpec, ScanConfig,
};

static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct TestDirectory {
    path: PathBuf,
}

impl TestDirectory {
    fn new(label: &str) -> Self {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "codeatlas-indexer-{label}-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).expect("test directory should be created");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn write(&self, relative: &str, contents: impl AsRef<[u8]>) {
        let path = self.path.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("fixture parent should be created");
        }
        fs::write(path, contents).expect("fixture should be written");
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[derive(Debug)]
struct CountingRustParser {
    calls: Arc<AtomicUsize>,
}

impl ParserAdapter for CountingRustParser {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn parse(&self, input: ParseInput<'_>) -> Result<ParsedFile, ParserError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if input.source.contains("BROKEN") {
            return Err(ParserError {
                path: input.path.clone(),
                message: "deliberate parser failure".to_owned(),
            });
        }
        let mut parsed = ParsedFile::empty(input.path.clone(), Language::Rust);
        parsed.symbols.push(ParsedSymbol {
            local_id: ParsedSymbolId(0),
            name: "run".to_owned(),
            qualified_name: "crate::run".to_owned(),
            kind: SymbolKind::Function,
            span: SourceSpan::new(1, 0, 1, 2).expect("valid span"),
            parent_id: None,
        });
        Ok(parsed)
    }
}

fn registry(calls: Arc<AtomicUsize>) -> ParserRegistry {
    let mut registry = ParserRegistry::new();
    registry.register_versioned(Arc::new(CountingRustParser { calls }), "test-parser-v1");
    registry
}

#[test]
fn scanner_honors_ignores_and_sorts_repository_paths() {
    let repository = TestDirectory::new("ignore");
    repository.write(".gitignore", "ignored.rs\n");
    repository.write(".ignore", "ignored.py\n");
    repository.write("ignored.rs", "fn ignored() {}\n");
    repository.write("ignored.py", "print('ignored')\n");
    repository.write("z.py", "print('z')\n");
    repository.write("src/a.rs", "fn a() {}\n");
    repository.write("target/generated.rs", "fn generated() {}\n");
    repository.write("node_modules/generated.py", "print('generated')\n");
    repository.write(".venv/generated.py", "print('generated')\n");

    let scan = RepositoryScanner::default()
        .scan(repository.path())
        .expect("repository should scan");
    let paths = scan
        .files
        .iter()
        .map(|file| file.path.as_str())
        .collect::<Vec<_>>();
    assert_eq!(paths, ["src/a.rs", "z.py"]);
    assert!(scan.skipped_files >= 2);
}

#[test]
fn scanner_skips_binary_large_and_symbolic_link_files() {
    let repository = TestDirectory::new("limits");
    repository.write("src/ok.rs", "fn ok() {}\n");
    repository.write("src/binary.rs", b"fn binary() {\0}\n");
    repository.write("src/large.py", vec![b'x'; 128]);

    #[cfg(unix)]
    std::os::unix::fs::symlink(
        repository.path().join("src/ok.rs"),
        repository.path().join("src/link.rs"),
    )
    .expect("symlink fixture should be created");

    let config = ScanConfig {
        max_file_size: 32,
        ..ScanConfig::default()
    };
    let scan = RepositoryScanner::new(config)
        .scan(repository.path())
        .expect("repository should scan");
    assert_eq!(scan.files.len(), 1);
    assert_eq!(scan.files[0].path.as_str(), "src/ok.rs");
    assert!(
        scan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "binary_file")
    );
    assert!(
        scan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "file_too_large")
    );
    #[cfg(unix)]
    assert!(
        scan.diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "symbolic_link")
    );
}

#[test]
fn parser_failures_are_diagnostics_and_other_files_continue() {
    let repository = TestDirectory::new("parse-errors");
    repository.write("bad.rs", "BROKEN\n");
    repository.write("good.rs", "fn good() {}\n");
    let parser_calls = Arc::new(AtomicUsize::new(0));
    let indexer = Indexer::new(ScanConfig::default(), registry(parser_calls.clone()));
    let progress = Mutex::new(Vec::new());
    let callback = |event| progress.lock().expect("progress lock").push(event);

    let report = indexer
        .index(
            repository.path(),
            RepositorySpec::new("fixture", "parse-errors"),
            Some(&callback),
        )
        .expect("one parser failure should not abort indexing");

    assert_eq!(parser_calls.load(Ordering::Relaxed), 2);
    assert_eq!(report.counts.scanned_files, 2);
    assert_eq!(report.counts.parsed_files, 1);
    assert_eq!(report.model.files.len(), 1);
    assert_eq!(report.model.files[0].path.as_str(), "good.rs");
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic
            .path
            .as_ref()
            .is_some_and(|path| path.as_str() == "bad.rs")
    }));
    let progress = progress.into_inner().expect("progress lock");
    assert!(
        progress
            .iter()
            .any(|event| event.phase == ProgressPhase::Scanning)
    );
    assert!(
        progress
            .iter()
            .any(|event| event.phase == ProgressPhase::Parsing)
    );
    assert!(
        progress
            .iter()
            .any(|event| event.phase == ProgressPhase::Indexing)
    );
}

#[test]
fn stable_ids_do_not_depend_on_file_or_local_symbol_order() {
    let first_a = parsed_with_local_call("src/a.rs", ParsedSymbolId(0), ParsedSymbolId(1), false);
    let first_b = parsed_with_local_call("src/b.rs", ParsedSymbolId(0), ParsedSymbolId(1), false);
    let second_a = parsed_with_local_call("src/a.rs", ParsedSymbolId(42), ParsedSymbolId(7), true);
    let second_b = parsed_with_local_call("src/b.rs", ParsedSymbolId(8), ParsedSymbolId(3), true);
    let assembler = IndexAssembler::new(RepositorySpec::new("fixture", "stable-ids"));

    let first = assembler
        .assemble([first_a, first_b])
        .expect("first model should assemble");
    let second = assembler
        .assemble([second_b, second_a])
        .expect("second model should assemble");

    assert_eq!(first, second);
    assert!(first.calls.iter().all(|call| {
        call.caller_id.is_some() && matches!(call.target, TargetResolution::Resolved(_))
    }));
}

#[test]
fn cache_round_trips_hits_and_invalidates_on_content_change() {
    let repository = TestDirectory::new("cache-repository");
    let cache = TestDirectory::new("cache-directory");
    repository.write("lib.rs", "fn first() {}\n");
    let parser_calls = Arc::new(AtomicUsize::new(0));
    let store = JsonIndexStore::new(cache.path());
    let indexer = Indexer::new(ScanConfig::default(), registry(parser_calls.clone()))
        .with_store(store.clone());
    let spec = RepositorySpec::new("fixture", "cache-fixture");

    let first = indexer
        .index(repository.path(), spec.clone(), None)
        .expect("first index should succeed");
    assert!(!first.cache_hit);
    assert_eq!(parser_calls.load(Ordering::Relaxed), 1);

    let second = indexer
        .index(repository.path(), spec.clone(), None)
        .expect("cached index should succeed");
    assert!(second.cache_hit);
    assert_eq!(first.model, second.model);
    assert_eq!(parser_calls.load(Ordering::Relaxed), 1);

    repository.write("lib.rs", "fn changed() {}\n");
    let third = indexer
        .index(repository.path(), spec, None)
        .expect("changed index should succeed");
    assert!(!third.cache_hit);
    assert_eq!(parser_calls.load(Ordering::Relaxed), 2);

    let cache_key = RepositorySpec::new("fixture", "cache-fixture")
        .repository_id()
        .to_string();
    let cached = store
        .read(&cache_key)
        .expect("cache should be readable")
        .expect("cache entry should exist");
    assert_eq!(cached.model, third.model);
}

#[test]
fn cache_store_rejects_an_invalid_core_schema() {
    let cache = TestDirectory::new("cache-schema");
    let store = JsonIndexStore::new(cache.path());
    let spec = RepositorySpec::new("fixture", "schema-fixture");
    let mut model = IndexAssembler::new(spec.clone())
        .assemble(Vec::new())
        .expect("empty model should assemble");
    model.schema_version = SchemaVersion::new(999);
    let cached = CachedIndex {
        fingerprint: "fingerprint".to_owned(),
        model,
        parsed_files: 0,
        skipped_files: 0,
        diagnostics: Vec::new(),
    };

    assert!(
        store
            .write(&spec.repository_id().to_string(), &cached)
            .is_err()
    );
}

#[test]
fn indexing_cancels_after_parse_without_writing_cache() {
    let repository = TestDirectory::new("cancel-repository");
    let cache = TestDirectory::new("cancel-cache");
    repository.write("a.rs", "fn a() {}\n");
    repository.write("b.rs", "fn b() {}\n");
    let parser_calls = Arc::new(AtomicUsize::new(0));
    let indexer = Indexer::new(ScanConfig::default(), registry(parser_calls.clone()))
        .with_store(JsonIndexStore::new(cache.path()));
    let cancellation = || parser_calls.load(Ordering::Relaxed) >= 1;

    let error = indexer
        .index_cancellable(
            repository.path(),
            RepositorySpec::new("fixture", "cancel-index"),
            None,
            Some(&cancellation),
        )
        .expect_err("index should observe cancellation after the first parse");

    assert!(matches!(error, IndexError::Cancelled));
    assert_eq!(parser_calls.load(Ordering::Relaxed), 1);
    assert_eq!(
        fs::read_dir(cache.path()).expect("cache directory").count(),
        0
    );
}

#[test]
fn cancelled_cache_serialization_does_not_publish_a_partial_entry() {
    let cache = TestDirectory::new("cancel-cache-write");
    let store = JsonIndexStore::new(cache.path());
    let spec = RepositorySpec::new("fixture", "cancel-cache-write");
    let cached = CachedIndex {
        fingerprint: "fingerprint".to_owned(),
        model: IndexAssembler::new(spec.clone())
            .assemble(Vec::new())
            .expect("empty model should assemble"),
        parsed_files: 0,
        skipped_files: 0,
        diagnostics: Vec::new(),
    };
    let checks = AtomicUsize::new(0);
    let cancellation = || checks.fetch_add(1, Ordering::Relaxed) >= 2;

    let error = store
        .write_cancellable(
            &spec.repository_id().to_string(),
            &cached,
            Some(&cancellation),
        )
        .expect_err("cache serialization should observe cancellation");

    assert!(matches!(error, IndexStoreError::Cancelled));
    assert_eq!(
        fs::read_dir(cache.path()).expect("cache directory").count(),
        0
    );
}

#[cfg(unix)]
#[test]
fn cache_store_uses_private_permissions_and_rejects_symlink_directory() {
    use std::os::unix::fs::{PermissionsExt as _, symlink};

    let root = TestDirectory::new("cache-safety");
    let cache = root.path().join("cache");
    let store = JsonIndexStore::new(&cache);
    let spec = RepositorySpec::new("fixture", "safe-cache");
    let cached = CachedIndex {
        fingerprint: "fingerprint".to_owned(),
        model: IndexAssembler::new(spec.clone())
            .assemble(Vec::new())
            .expect("empty model should assemble"),
        parsed_files: 0,
        skipped_files: 0,
        diagnostics: Vec::new(),
    };
    store
        .write(&spec.repository_id().to_string(), &cached)
        .expect("cache should save");
    assert_eq!(
        fs::metadata(&cache)
            .expect("cache metadata")
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let entry = fs::read_dir(&cache)
        .expect("cache directory")
        .next()
        .expect("cache entry")
        .expect("read cache entry")
        .path();
    assert_eq!(
        fs::metadata(&entry)
            .expect("entry metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let escaped_file = root.path().join("escaped-cache.json");
    fs::write(&escaped_file, b"do not replace").expect("escaped file fixture");
    fs::remove_file(&entry).expect("cache entry should be removable");
    symlink(&escaped_file, &entry).expect("cache entry symlink");
    assert!(matches!(
        store.read(&spec.repository_id().to_string()),
        Err(IndexStoreError::UnsafeStoragePath { .. })
    ));
    assert!(matches!(
        store.write(&spec.repository_id().to_string(), &cached),
        Err(IndexStoreError::UnsafeStoragePath { .. })
    ));
    assert_eq!(
        fs::read_to_string(&escaped_file).expect("escaped file should remain readable"),
        "do not replace"
    );

    let escaped = root.path().join("escaped");
    let linked = root.path().join("linked");
    fs::create_dir(&escaped).expect("escaped directory");
    symlink(&escaped, &linked).expect("cache directory symlink");
    let error = JsonIndexStore::new(linked)
        .write(&spec.repository_id().to_string(), &cached)
        .expect_err("symlinked cache directory must be rejected");
    assert!(matches!(error, IndexStoreError::UnsafeStoragePath { .. }));
    assert_eq!(fs::read_dir(escaped).expect("escaped directory").count(), 0);
}

fn parsed_with_local_call(
    path: &str,
    caller_id: ParsedSymbolId,
    target_id: ParsedSymbolId,
    reverse_symbols: bool,
) -> ParsedFile {
    let path = RepositoryPath::new(path).expect("canonical path");
    let mut parsed = ParsedFile::empty(path, Language::Rust);
    let caller = ParsedSymbol {
        local_id: caller_id,
        name: "caller".to_owned(),
        qualified_name: "crate::caller".to_owned(),
        kind: SymbolKind::Function,
        span: SourceSpan::new(1, 0, 1, 10).expect("valid span"),
        parent_id: None,
    };
    let target = ParsedSymbol {
        local_id: target_id,
        name: "target".to_owned(),
        qualified_name: "crate::target".to_owned(),
        kind: SymbolKind::Function,
        span: SourceSpan::new(2, 0, 2, 10).expect("valid span"),
        parent_id: None,
    };
    parsed.symbols = if reverse_symbols {
        vec![target, caller]
    } else {
        vec![caller, target]
    };
    parsed.calls.push(ParsedCall {
        caller_id: Some(caller_id),
        target: ParsedTarget::Local(target_id),
        span: SourceSpan::new(1, 2, 1, 8).expect("valid span"),
        confidence: None,
    });
    parsed
}
