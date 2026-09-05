#![allow(clippy::expect_used)]

use std::{
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use codeatlas_agent::{ModelPricing, SESSION_SCHEMA_VERSION, SessionState, SessionStore};
use codeatlas_core::{
    AgentAnswer, AnswerId, Claim, ClaimId, ClaimKind, DiagramDecision, Evidence, EvidenceId,
    FileId, ModelUsage, Progress, ProgressPhase, RepositoryId, RepositoryPath, RequestId,
    SessionId, SourceSpan, TokenUsage, ToolCall, ToolCallId, WorkflowEvent,
};

fn evidence() -> Evidence {
    Evidence {
        id: EvidenceId::from_stable_parts(&["session-evidence"]),
        file_id: FileId::from_stable_parts(&["src/lib.rs"]),
        path: RepositoryPath::new("src/lib.rs").expect("valid path"),
        span: SourceSpan::new(1, 0, 1, 12).expect("valid span"),
        symbol_id: None,
        excerpt: Some("pub fn run()".to_owned()),
    }
}

fn answer() -> AgentAnswer {
    let item = evidence();
    AgentAnswer {
        id: AnswerId::from_stable_parts(&["session-answer"]),
        text: "run is defined here".to_owned(),
        claims: vec![Claim {
            id: ClaimId::from_stable_parts(&["session-claim"]),
            kind: ClaimKind::Fact,
            text: "run is defined here".to_owned(),
            evidence_ids: vec![item.id],
        }],
        evidence: vec![item],
        call_paths: Vec::new(),
        diagram: DiagramDecision::NotNeeded {
            reason: "A direct location answer does not need a diagram.".to_owned(),
        },
        usage: Some(ModelUsage {
            tokens: TokenUsage {
                input_tokens: 1_000_000,
                output_tokens: 500_000,
                cached_input_tokens: 500_000,
                total_tokens: 1_500_000,
            },
            cost: None,
        }),
    }
}

fn temporary_directory(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "codeatlas-agent-{label}-{}-{}",
        std::process::id(),
        SessionId::from_stable_parts(&[label])
    ))
}

fn current_unix_ms() -> u64 {
    let milliseconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock should be after Unix epoch")
        .as_millis();
    u64::try_from(milliseconds).expect("current Unix milliseconds should fit in u64")
}

#[test]
fn new_session_sets_matching_current_timestamps() {
    let before = current_unix_ms();
    let state = SessionState::new(
        SessionId::from_stable_parts(&["timestamp-session"]),
        RepositoryId::from_stable_parts(&["timestamp-repository"]),
    );
    let after = current_unix_ms();

    assert_eq!(state.created_at_unix_ms, state.updated_at_unix_ms);
    assert!((before..=after).contains(&state.created_at_unix_ms));
}

#[test]
fn successful_appends_update_only_the_updated_timestamp() {
    let mut state = SessionState::new(
        SessionId::from_stable_parts(&["append-timestamp-session"]),
        RepositoryId::from_stable_parts(&["append-timestamp-repository"]),
    );
    state.created_at_unix_ms = 123;
    state.updated_at_unix_ms = 123;

    let before = current_unix_ms();
    state
        .append_turn(
            RequestId::from_stable_parts(&["append-timestamp-request"]),
            "Where is run?",
            answer(),
        )
        .expect("valid turn");
    let after = current_unix_ms();

    assert_eq!(state.created_at_unix_ms, 123);
    assert!((before..=after).contains(&state.updated_at_unix_ms));

    state.updated_at_unix_ms = 123;
    let before = current_unix_ms();
    state
        .append_turn_with_workflow(
            RequestId::from_stable_parts(&["workflow-append-timestamp-request"]),
            "How does run work?",
            answer(),
            Vec::new(),
        )
        .expect("valid workflow turn");
    let after = current_unix_ms();

    assert_eq!(state.created_at_unix_ms, 123);
    assert!((before..=after).contains(&state.updated_at_unix_ms));
}

#[test]
fn session_round_trips_atomically_with_history_usage_and_cost() {
    let directory = temporary_directory("roundtrip");
    let _ = fs::remove_dir_all(&directory);
    let session_id = SessionId::from_stable_parts(&["roundtrip-session"]);
    let repository_id = RepositoryId::from_stable_parts(&["roundtrip-repository"]);
    let request_id = RequestId::from_stable_parts(&["roundtrip-request"]);
    let tool_call_id = ToolCallId::from_stable_parts(&["roundtrip-tool"]);
    let workflow = vec![
        WorkflowEvent::Progress(Progress {
            phase: ProgressPhase::Searching,
            message: "finding run".to_owned(),
            completed: Some(1),
            total: Some(2),
        }),
        WorkflowEvent::ToolCallStarted(ToolCall {
            id: tool_call_id,
            name: "find_symbol".to_owned(),
            arguments: serde_json::json!({"query": "run"}),
        }),
        WorkflowEvent::ToolCallCompleted {
            call_id: tool_call_id,
            output: "1 matching symbol".to_owned(),
            is_error: false,
        },
    ];
    let mut state = SessionState::new(session_id, repository_id);
    state
        .append_turn_with_workflow(request_id, "Where is run?", answer(), workflow.clone())
        .expect("valid turn");
    let pricing = ModelPricing {
        currency: "USD".to_owned(),
        input_per_million: 2.0,
        cached_input_per_million: Some(0.5),
        output_per_million: 4.0,
    };
    state.reprice(&pricing).expect("valid pricing");
    let store = SessionStore::new(&directory);

    store.save(&state).expect("session should save");
    let restored = store.load(session_id).expect("session should load");

    assert_eq!(restored, state);
    assert_eq!(restored.schema_version, SESSION_SCHEMA_VERSION);
    assert_eq!(restored.turns[0].workflow, workflow);
    assert_eq!(restored.conversation_history().len(), 2);
    assert_eq!(restored.usage.tokens.total_tokens, 1_500_000);
    let restored_cost = restored.usage.cost.as_ref().expect("estimated cost").amount;
    assert!((restored_cost - 3.25).abs() < f64::EPSILON);
    let entries = fs::read_dir(&directory)
        .expect("session directory")
        .collect::<Result<Vec<_>, _>>()
        .expect("directory entries");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].path(), store.session_path(session_id));

    fs::remove_dir_all(directory).expect("temporary directory cleanup");
}

#[test]
fn legacy_session_json_without_workflow_loads_with_an_empty_trace() {
    let directory = temporary_directory("legacy-workflow");
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("session directory");
    let session_id = SessionId::from_stable_parts(&["legacy-workflow-session"]);
    let mut state = SessionState::new(
        session_id,
        RepositoryId::from_stable_parts(&["legacy-workflow-repository"]),
    );
    state
        .append_turn(
            RequestId::from_stable_parts(&["legacy-workflow-request"]),
            "Where is run?",
            answer(),
        )
        .expect("valid legacy turn");
    let mut encoded = serde_json::to_value(&state).expect("session should serialize");
    encoded["schema_version"] = serde_json::json!(1);
    encoded["turns"][0]
        .as_object_mut()
        .expect("turn should be an object")
        .remove("workflow");
    encoded
        .as_object_mut()
        .expect("session should be an object")
        .remove("created_at_unix_ms");
    encoded
        .as_object_mut()
        .expect("session should be an object")
        .remove("updated_at_unix_ms");
    let store = SessionStore::new(&directory);
    fs::write(
        store.session_path(session_id),
        serde_json::to_vec_pretty(&encoded).expect("legacy JSON should serialize"),
    )
    .expect("legacy session should be written");

    let restored = store.load(session_id).expect("legacy session should load");
    assert_eq!(restored.schema_version, SESSION_SCHEMA_VERSION);
    assert!(restored.turns[0].workflow.is_empty());
    assert_eq!(restored.created_at_unix_ms, 0);
    assert_eq!(restored.updated_at_unix_ms, 0);

    fs::remove_dir_all(directory).expect("temporary directory cleanup");
}

#[test]
fn session_loader_normalizes_partial_and_inverted_timestamps() {
    let directory = temporary_directory("normalize-timestamps");
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("session directory");
    let session_id = SessionId::from_stable_parts(&["normalize-timestamps-session"]);
    let state = SessionState::new(
        session_id,
        RepositoryId::from_stable_parts(&["normalize-timestamps-repository"]),
    );
    let store = SessionStore::new(&directory);
    let mut encoded = serde_json::to_value(&state).expect("session should serialize");
    encoded["created_at_unix_ms"] = serde_json::json!(500);
    encoded["updated_at_unix_ms"] = serde_json::json!(400);
    fs::write(
        store.session_path(session_id),
        serde_json::to_vec_pretty(&encoded).expect("session JSON should serialize"),
    )
    .expect("session should be written");

    let restored = store.load(session_id).expect("session should load");
    assert_eq!(restored.created_at_unix_ms, 500);
    assert_eq!(restored.updated_at_unix_ms, 500);

    encoded["created_at_unix_ms"] = serde_json::json!(0);
    encoded["updated_at_unix_ms"] = serde_json::json!(600);
    fs::write(
        store.session_path(session_id),
        serde_json::to_vec_pretty(&encoded).expect("session JSON should serialize"),
    )
    .expect("session should be written");

    let restored = store.load(session_id).expect("session should load");
    assert_eq!(restored.created_at_unix_ms, 600);
    assert_eq!(restored.updated_at_unix_ms, 600);

    encoded["created_at_unix_ms"] = serde_json::json!(700);
    encoded["updated_at_unix_ms"] = serde_json::json!(0);
    fs::write(
        store.session_path(session_id),
        serde_json::to_vec_pretty(&encoded).expect("session JSON should serialize"),
    )
    .expect("session should be written");

    let restored = store.load(session_id).expect("session should load");
    assert_eq!(restored.created_at_unix_ms, 700);
    assert_eq!(restored.updated_at_unix_ms, 700);

    fs::remove_dir_all(directory).expect("temporary directory cleanup");
}

#[test]
fn future_session_schema_is_rejected_explicitly() {
    let directory = temporary_directory("future-schema");
    let _ = fs::remove_dir_all(&directory);
    fs::create_dir_all(&directory).expect("session directory");
    let session_id = SessionId::from_stable_parts(&["future-schema-session"]);
    let state = SessionState::new(
        session_id,
        RepositoryId::from_stable_parts(&["future-schema-repository"]),
    );
    let mut encoded = serde_json::to_value(state).expect("session should serialize");
    encoded["schema_version"] = serde_json::json!(SESSION_SCHEMA_VERSION + 1);
    let store = SessionStore::new(&directory);
    fs::write(
        store.session_path(session_id),
        serde_json::to_vec_pretty(&encoded).expect("future JSON should serialize"),
    )
    .expect("future session should be written");

    let error = store
        .load(session_id)
        .expect_err("future schema must be rejected");
    assert!(error.to_string().contains("unsupported session schema"));

    fs::remove_dir_all(directory).expect("temporary directory cleanup");
}

#[test]
fn legacy_agent_answer_json_without_diagram_uses_the_core_default() {
    let mut encoded = serde_json::to_value(answer()).expect("answer should serialize");
    encoded
        .as_object_mut()
        .expect("answer should be an object")
        .remove("diagram");

    let restored: AgentAnswer =
        serde_json::from_value(encoded).expect("legacy answer should deserialize");

    let DiagramDecision::NotNeeded { reason } = &restored.diagram else {
        panic!("legacy answer should default to not-needed");
    };
    assert!(reason.contains("legacy"));
    restored
        .validate_evidence()
        .expect("legacy answer should remain valid");
}

#[test]
fn runtime_secret_debug_output_and_persisted_sessions_are_redacted() {
    let secret_value = "sk-test-never-persist";
    let secret = codeatlas_agent::SecretString::new(secret_value);
    assert!(!format!("{secret:?}").contains(secret_value));

    let headers =
        codeatlas_agent::RuntimeSecretHeaders::bearer(secret).expect("valid authorization header");
    let headers_debug = format!("{headers:?}");
    assert!(!headers_debug.contains(secret_value));
    assert!(headers_debug.contains("REDACTED"));

    let directory = temporary_directory("secret");
    let _ = fs::remove_dir_all(&directory);
    let session_id = SessionId::from_stable_parts(&["secret-session"]);
    let state = SessionState::new(
        session_id,
        RepositoryId::from_stable_parts(&["secret-repository"]),
    );
    let store = SessionStore::new(&directory);
    store.save(&state).expect("session should save");
    let serialized = fs::read_to_string(store.session_path(session_id)).expect("session JSON");

    assert!(!serialized.contains(secret_value));
    assert!(serialized.contains(&format!("\"schema_version\": {SESSION_SCHEMA_VERSION}")));
    fs::remove_dir_all(directory).expect("temporary directory cleanup");
}

#[cfg(unix)]
#[test]
fn session_store_rejects_a_symlinked_storage_directory() {
    use std::os::unix::fs::symlink;

    let root = temporary_directory("symlink-store");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("test root");
    let escaped = root.join("escaped");
    let linked = root.join("sessions");
    fs::create_dir(&escaped).expect("escaped directory");
    symlink(&escaped, &linked).expect("session directory symlink");
    let session_id = SessionId::from_stable_parts(&["symlink-session"]);
    let store = SessionStore::new(&linked);
    let state = SessionState::new(
        session_id,
        RepositoryId::from_stable_parts(&["symlink-repository"]),
    );

    let error = store.save(&state).expect_err("symlink must be rejected");
    assert!(error.to_string().contains("unsafe session storage path"));
    assert!(!escaped.join(format!("{session_id}.json")).exists());

    fs::remove_dir_all(root).expect("temporary directory cleanup");
}

#[test]
fn explicit_pricing_accounts_for_cached_tokens_without_double_charging() {
    let pricing = ModelPricing {
        currency: "USD".to_owned(),
        input_per_million: 2.0,
        cached_input_per_million: Some(0.5),
        output_per_million: 4.0,
    };
    let cost = pricing
        .estimate(&TokenUsage {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            cached_input_tokens: 500_000,
            total_tokens: 1_500_000,
        })
        .expect("valid pricing");

    assert_eq!(cost.currency, "USD");
    assert!((cost.amount - 3.25).abs() < f64::EPSILON);
    assert!(cost.estimated);
}
