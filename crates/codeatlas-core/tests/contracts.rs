use codeatlas_core::{
    AgentAnswer, AnswerId, AppCommand, AppEvent, CallEdge, CallEdgeId, Claim, ClaimId, ClaimKind,
    Diagram, DiagramArtifact, DiagramDecision, DiagramEdge, DiagramId, DiagramKind, DiagramNode,
    Evidence, EvidenceId, EvidenceValidationError, FileId, FileInfo, Language, ModelUsage,
    Progress, ProgressPhase, RepositoryId, RepositoryModel, RepositoryPath, RequestId,
    SessionContext, SessionId, SessionSummary, SessionTask, SessionTaskSummary, SourceSpan, Symbol,
    SymbolId, SymbolKind, TargetResolution, TokenUsage, ToolCall, ToolCallId, UnresolvedTarget,
    WorkflowEvent,
};

fn id_parts() -> [&'static str; 3] {
    ["src/lib.rs", "function", "codeatlas_core::run"]
}

#[test]
fn validation_errors_distinguish_optional_diagram_failures() {
    assert!(EvidenceValidationError::EmptyDiagramTitle.is_diagram_error());
    assert!(
        EvidenceValidationError::DiagramNodeClaimWithoutLinkedEvidence {
            node_index: 3,
            claim_index: 1,
        }
        .is_diagram_error()
    );
    assert!(
        !EvidenceValidationError::FactWithoutEvidence {
            claim_id: ClaimId::from_stable_parts(&["strict-fact"]),
        }
        .is_diagram_error()
    );
}

fn diagram_answer(kind: DiagramKind) -> AgentAnswer {
    let evidence = Evidence {
        id: EvidenceId::from_stable_parts(&["diagram", "evidence"]),
        file_id: FileId::from_stable_parts(&["src/lib.rs"]),
        path: RepositoryPath::new("src/lib.rs").expect("canonical path"),
        span: SourceSpan::new(1, 0, 3, 1).expect("valid source span"),
        symbol_id: Some(SymbolId::from_stable_parts(&["diagram", "symbol"])),
        excerpt: Some("fn run() { service(); }".to_owned()),
    };
    let evidence_id = evidence.id;
    let claim_id = ClaimId::from_stable_parts(&["diagram", "claim"]);
    let node = |id: &str, label: &str| DiagramNode {
        id: id.to_owned(),
        label: label.to_owned(),
        claim_ids: vec![claim_id],
        evidence_ids: vec![evidence_id],
    };

    AgentAnswer {
        id: AnswerId::from_stable_parts(&["diagram", "answer"]),
        text: "The runtime delegates to the service.".to_owned(),
        claims: vec![Claim {
            id: claim_id,
            kind: ClaimKind::Fact,
            text: "The runtime delegates to the service.".to_owned(),
            evidence_ids: vec![evidence_id],
        }],
        evidence: vec![evidence],
        call_paths: Vec::new(),
        diagram: DiagramDecision::Needed {
            reason: "The interaction is easier to follow visually.".to_owned(),
            diagram: Diagram {
                kind,
                title: "Runtime delegation".to_owned(),
                nodes: vec![node("runtime", "Runtime"), node("service", "Service")],
                edges: vec![DiagramEdge {
                    source: "runtime".to_owned(),
                    target: "service".to_owned(),
                    label: "delegates".to_owned(),
                    claim_ids: vec![claim_id],
                    evidence_ids: vec![evidence_id],
                }],
                artifact: Some(DiagramArtifact {
                    id: DiagramId::from_stable_parts(&["diagram", "artifact"]),
                    path: "artifacts/runtime.svg".to_owned(),
                    media_type: "image/svg+xml".to_owned(),
                    byte_size: 512,
                }),
            },
        },
        usage: None,
    }
}

fn answer_diagram_mut(answer: &mut AgentAnswer) -> &mut Diagram {
    let DiagramDecision::Needed { diagram, .. } = &mut answer.diagram else {
        panic!("test answer should require a diagram");
    };
    diagram
}

#[test]
fn stable_ids_are_deterministic_and_namespaced() {
    let first = SymbolId::from_stable_parts(&id_parts());
    let second = SymbolId::from_stable_parts(&id_parts());
    let file = FileId::from_stable_parts(&id_parts());

    assert_eq!(first, second);
    assert_eq!(first.to_string(), "2ea8cb278da7e841842a5c8659723cf8");
    assert_ne!(first.to_string(), file.to_string());

    let encoded = serde_json::to_string(&first).expect("ID should serialize");
    let decoded: SymbolId = serde_json::from_str(&encoded).expect("ID should deserialize");
    assert_eq!(decoded, first);
}

#[test]
fn source_span_validates_bounds_and_deserialization() {
    let span = SourceSpan::new(1, 0, 2, 4).expect("valid source span");
    assert_eq!(span.start().line(), 1);
    assert_eq!(span.start().column(), 0);
    assert_eq!(span.end().line(), 2);
    assert_eq!(span.end().column(), 4);

    assert!(SourceSpan::new(0, 0, 1, 0).is_err());
    assert!(SourceSpan::new(3, 0, 2, 0).is_err());
    assert!(SourceSpan::new(2, 8, 2, 7).is_err());

    let invalid = r#"{"start":{"line":2,"column":4},"end":{"line":2,"column":3}}"#;
    assert!(serde_json::from_str::<SourceSpan>(invalid).is_err());
}

#[test]
fn repository_paths_are_canonical_even_after_deserialization() {
    let path = RepositoryPath::new("src/parser/mod.rs").expect("canonical path");
    let encoded = serde_json::to_string(&path).expect("path should serialize");
    assert_eq!(encoded, r#""src/parser/mod.rs""#);
    assert_eq!(
        serde_json::from_str::<RepositoryPath>(&encoded).expect("path should deserialize"),
        path
    );

    for invalid in [r#""/src/lib.rs""#, r#""src\\lib.rs""#, r#""src/../lib.rs""#] {
        assert!(serde_json::from_str::<RepositoryPath>(invalid).is_err());
    }
}

#[test]
fn repository_ir_round_trips_and_preserves_unresolved_calls() {
    let path = RepositoryPath::new("src/lib.rs").expect("canonical path");
    let repository_id = RepositoryId::from_stable_parts(&["codeatlas"]);
    let file_id = FileId::from_stable_parts(&[path.as_str()]);
    let caller_id = SymbolId::from_stable_parts(&[path.as_str(), "function", "run"]);
    let call_id = CallEdgeId::from_stable_parts(&[&caller_id.to_string(), "external_call", "1:12"]);

    let mut model = RepositoryModel::empty(repository_id, "codeatlas");
    model.files.push(FileInfo {
        id: file_id,
        path,
        language: Language::Rust,
    });
    model.symbols.push(Symbol {
        id: caller_id,
        name: "run".to_owned(),
        qualified_name: "crate::run".to_owned(),
        kind: SymbolKind::Function,
        file_id,
        module_id: None,
        span: SourceSpan::new(1, 0, 3, 1).expect("valid symbol span"),
        parent_id: None,
    });
    model.calls.push(CallEdge {
        id: call_id,
        source_file_id: file_id,
        caller_id: None,
        target: TargetResolution::Unresolved(UnresolvedTarget {
            name: "external_call".to_owned(),
            reason: Some("dependency not indexed".to_owned()),
        }),
        span: SourceSpan::new(2, 4, 2, 19).expect("valid call span"),
        confidence: None,
    });

    let encoded = serde_json::to_string(&model).expect("model should serialize");
    let decoded: RepositoryModel =
        serde_json::from_str(&encoded).expect("model should deserialize");

    assert_eq!(decoded, model);
    assert!(matches!(
        decoded.calls[0].target,
        TargetResolution::Unresolved(_)
    ));
    assert_eq!(decoded.calls[0].caller_id, None);
}

#[test]
fn fact_without_evidence_is_reported() {
    let claim_id = ClaimId::from_stable_parts(&["answer", "claim-1"]);
    let claim = Claim {
        id: claim_id,
        kind: ClaimKind::Fact,
        text: "run calls external_call".to_owned(),
        evidence_ids: Vec::new(),
    };

    assert!(claim.is_unsupported_fact());
    assert_eq!(
        claim.validate_evidence(),
        Err(EvidenceValidationError::FactWithoutEvidence { claim_id })
    );

    let answer = AgentAnswer {
        id: AnswerId::from_stable_parts(&["answer"]),
        text: claim.text.clone(),
        claims: vec![claim],
        evidence: Vec::new(),
        call_paths: Vec::new(),
        diagram: DiagramDecision::default(),
        usage: None,
    };
    assert!(matches!(
        answer.validate_evidence(),
        Err(EvidenceValidationError::FactWithoutEvidence { .. })
    ));
}

#[test]
fn diagram_decisions_round_trip_all_kinds() {
    for (kind, serialized_kind) in [
        (DiagramKind::Architecture, "architecture"),
        (DiagramKind::Flow, "flow"),
        (DiagramKind::Relationship, "relationship"),
    ] {
        let mut answer = diagram_answer(kind);
        if kind == DiagramKind::Flow {
            answer_diagram_mut(&mut answer).artifact = None;
        }

        let encoded = serde_json::to_value(&answer).expect("diagram answer should serialize");
        assert_eq!(encoded["diagram"]["decision"], "needed");
        assert_eq!(encoded["diagram"]["diagram"]["kind"], serialized_kind);
        if kind == DiagramKind::Flow {
            assert!(
                encoded["diagram"]["diagram"]
                    .as_object()
                    .expect("diagram should be an object")
                    .get("artifact")
                    .is_none()
            );
        }

        let decoded: AgentAnswer =
            serde_json::from_value(encoded).expect("diagram answer should deserialize");
        assert_eq!(decoded, answer);
        assert_eq!(decoded.validate_evidence(), Ok(()));
    }
}

#[test]
fn legacy_agent_answer_defaults_to_not_needing_a_diagram() {
    let answer = diagram_answer(DiagramKind::Architecture);
    let mut encoded = serde_json::to_value(answer).expect("answer should serialize");
    encoded
        .as_object_mut()
        .expect("answer should be an object")
        .remove("diagram");

    let decoded: AgentAnswer =
        serde_json::from_value(encoded).expect("legacy answer should deserialize");
    let DiagramDecision::NotNeeded { reason } = &decoded.diagram else {
        panic!("legacy answer should default to a not-needed decision");
    };
    assert!(reason.contains("legacy"));
    assert_eq!(decoded.validate_evidence(), Ok(()));
}

#[test]
fn diagram_decision_requires_a_nonempty_reason() {
    let mut answer = diagram_answer(DiagramKind::Architecture);
    answer.diagram = DiagramDecision::NotNeeded {
        reason: "  ".to_owned(),
    };

    assert_eq!(
        answer.validate_evidence(),
        Err(EvidenceValidationError::EmptyDiagramDecisionReason)
    );
}

#[test]
fn duplicate_and_dangling_diagram_nodes_are_reported_by_index() {
    let mut duplicate = diagram_answer(DiagramKind::Architecture);
    answer_diagram_mut(&mut duplicate).nodes[1].id = "runtime".to_owned();
    assert_eq!(
        duplicate.validate_evidence(),
        Err(EvidenceValidationError::DuplicateDiagramNodeId {
            first_node_index: 0,
            duplicate_node_index: 1,
        })
    );

    let mut dangling = diagram_answer(DiagramKind::Architecture);
    answer_diagram_mut(&mut dangling).edges[0].target = "missing".to_owned();
    assert_eq!(
        dangling.validate_evidence(),
        Err(EvidenceValidationError::UnknownDiagramEdgeTarget { edge_index: 0 })
    );
}

#[test]
fn diagram_elements_reject_unknown_claims() {
    let mut answer = diagram_answer(DiagramKind::Flow);
    answer_diagram_mut(&mut answer).nodes[0].claim_ids =
        vec![ClaimId::from_stable_parts(&["diagram", "unknown-claim"])];

    assert_eq!(
        answer.validate_evidence(),
        Err(EvidenceValidationError::UnknownDiagramNodeClaim {
            node_index: 0,
            claim_index: 0,
        })
    );
}

#[test]
fn diagram_elements_reject_non_fact_claims() {
    let mut answer = diagram_answer(DiagramKind::Relationship);
    let claim_id = ClaimId::from_stable_parts(&["diagram", "inference"]);
    answer.claims.push(Claim {
        id: claim_id,
        kind: ClaimKind::Inference,
        text: "The delegation may be intentional layering.".to_owned(),
        evidence_ids: vec![answer.evidence[0].id],
    });
    answer_diagram_mut(&mut answer).edges[0].claim_ids = vec![claim_id];

    assert_eq!(
        answer.validate_evidence(),
        Err(EvidenceValidationError::NonFactDiagramEdgeClaim {
            edge_index: 0,
            claim_index: 0,
        })
    );
}

#[test]
fn diagram_evidence_must_cover_every_linked_fact_claim() {
    let mut answer = diagram_answer(DiagramKind::Architecture);
    let first_claim_id = answer.claims[0].id;
    let first_evidence_id = answer.evidence[0].id;
    let second_evidence_id = EvidenceId::from_stable_parts(&["diagram", "second-evidence"]);
    let mut second_evidence = answer.evidence[0].clone();
    second_evidence.id = second_evidence_id;
    answer.evidence.push(second_evidence);

    let second_claim_id = ClaimId::from_stable_parts(&["diagram", "second-claim"]);
    answer.claims.push(Claim {
        id: second_claim_id,
        kind: ClaimKind::Fact,
        text: "The service has a separate implementation.".to_owned(),
        evidence_ids: vec![second_evidence_id],
    });
    let node = &mut answer_diagram_mut(&mut answer).nodes[0];
    node.claim_ids = vec![first_claim_id, second_claim_id];
    node.evidence_ids = vec![first_evidence_id];

    assert_eq!(
        answer.validate_evidence(),
        Err(
            EvidenceValidationError::DiagramNodeClaimWithoutLinkedEvidence {
                node_index: 0,
                claim_index: 1,
            }
        )
    );

    answer_diagram_mut(&mut answer).nodes[0]
        .evidence_ids
        .push(second_evidence_id);
    answer
        .validate_evidence()
        .expect("each fact may be covered by its own directly linked evidence");
}

#[test]
fn application_event_round_trips() {
    let event = AppEvent::Progress {
        request_id: RequestId::from_stable_parts(&["session", "request-1"]),
        progress: Progress {
            phase: ProgressPhase::Tracing,
            message: "following call graph".to_owned(),
            completed: Some(3),
            total: Some(8),
        },
    };

    let encoded = serde_json::to_string(&event).expect("event should serialize");
    let decoded: AppEvent = serde_json::from_str(&encoded).expect("event should deserialize");
    assert_eq!(decoded, event);
}

#[test]
fn load_source_command_round_trips() {
    let command = AppCommand::LoadSource {
        request_id: RequestId::from_stable_parts(&["source", "request"]),
        repository_id: RepositoryId::from_stable_parts(&["source", "repository"]),
        path: RepositoryPath::new("src/lib.rs").expect("canonical path"),
        start_line: 12,
        end_line: Some(24),
    };

    let encoded = serde_json::to_string(&command).expect("command should serialize");
    let decoded: AppCommand = serde_json::from_str(&encoded).expect("command should deserialize");
    assert_eq!(decoded, command);
}

#[test]
fn source_loaded_event_round_trips() {
    let event = AppEvent::SourceLoaded {
        request_id: RequestId::from_stable_parts(&["source", "request"]),
        repository_id: RepositoryId::from_stable_parts(&["source", "repository"]),
        path: RepositoryPath::new("src/lib.rs").expect("canonical path"),
        start_line: 12,
        end_line: 13,
        content: "fn load_source() {}\n".to_owned(),
    };

    let encoded = serde_json::to_string(&event).expect("event should serialize");
    let decoded: AppEvent = serde_json::from_str(&encoded).expect("event should deserialize");
    assert_eq!(decoded, event);
}

#[test]
fn diagram_open_command_and_event_round_trip() {
    let request_id = RequestId::from_stable_parts(&["diagram", "open-request"]);
    let diagram_id = DiagramId::from_stable_parts(&["diagram", "open-artifact"]);
    let command = AppCommand::OpenDiagram {
        request_id,
        diagram_id,
    };
    let encoded = serde_json::to_string(&command).expect("command should serialize");
    let decoded: AppCommand = serde_json::from_str(&encoded).expect("command should deserialize");
    assert_eq!(decoded, command);

    let event = AppEvent::DiagramOpened {
        request_id,
        diagram_id,
    };
    let encoded = serde_json::to_string(&event).expect("event should serialize");
    let decoded: AppEvent = serde_json::from_str(&encoded).expect("event should deserialize");
    assert_eq!(decoded, event);
}

#[test]
fn session_history_commands_and_context_events_round_trip() {
    let request_id = RequestId::from_stable_parts(&["history", "request"]);
    let session_id = SessionId::from_stable_parts(&["history", "session"]);
    let repository_id = RepositoryId::from_stable_parts(&["history", "repository"]);
    for command in [
        AppCommand::ListSessions {
            request_id,
            repository_id: Some(repository_id),
        },
        AppCommand::LoadSession {
            request_id,
            session_id,
        },
    ] {
        let encoded = serde_json::to_string(&command).expect("command should serialize");
        let decoded: AppCommand =
            serde_json::from_str(&encoded).expect("command should deserialize");
        assert_eq!(decoded, command);
    }

    let task_request = RequestId::from_stable_parts(&["history", "task"]);
    let context = SessionContext {
        schema_version: 1,
        session_id,
        repository_id,
        tasks: vec![SessionTask {
            request_id: task_request,
            question: "How does run work?".to_owned(),
            answer: diagram_answer(DiagramKind::Flow),
            workflow: vec![
                WorkflowEvent::Progress(Progress {
                    phase: ProgressPhase::Searching,
                    message: "finding run".to_owned(),
                    completed: None,
                    total: None,
                }),
                WorkflowEvent::ToolCallStarted(ToolCall {
                    id: ToolCallId::from_stable_parts(&["history", "tool"]),
                    name: "find_symbol".to_owned(),
                    arguments: serde_json::json!({"query": "run"}),
                }),
            ],
        }],
        usage: ModelUsage {
            tokens: TokenUsage::default(),
            cost: None,
        },
        created_at_unix_ms: 1_750_000_000_000,
        updated_at_unix_ms: 1_750_000_001_000,
        json_path: "/data/sessions/history.json".to_owned(),
    };
    let context_json = serde_json::to_value(&context).expect("context should serialize");
    assert_eq!(context_json["created_at_unix_ms"], 1_750_000_000_000_u64);
    assert_eq!(context_json["updated_at_unix_ms"], 1_750_000_001_000_u64);
    let loaded = AppEvent::SessionLoaded {
        request_id,
        session: context,
    };
    let encoded = serde_json::to_string(&loaded).expect("event should serialize");
    let decoded: AppEvent = serde_json::from_str(&encoded).expect("event should deserialize");
    assert_eq!(decoded, loaded);

    let summary = SessionSummary {
        session_id,
        repository_id,
        tasks: vec![SessionTaskSummary {
            request_id: task_request,
            question: "How does run work?".to_owned(),
        }],
        created_at_unix_ms: 1_750_000_000_000,
        updated_at_unix_ms: 1_750_000_001_000,
        json_path: "/data/sessions/history.json".to_owned(),
    };
    let summary_json = serde_json::to_value(&summary).expect("summary should serialize");
    assert_eq!(summary_json["created_at_unix_ms"], 1_750_000_000_000_u64);
    assert_eq!(summary_json["updated_at_unix_ms"], 1_750_000_001_000_u64);
    let listed = AppEvent::SessionsListed {
        request_id,
        sessions: vec![summary],
    };
    let encoded = serde_json::to_string(&listed).expect("event should serialize");
    let decoded: AppEvent = serde_json::from_str(&encoded).expect("event should deserialize");
    assert_eq!(decoded, listed);
}

#[test]
fn legacy_session_contracts_default_missing_timestamps_to_zero() {
    let session_id = SessionId::from_stable_parts(&["legacy", "session"]);
    let repository_id = RepositoryId::from_stable_parts(&["legacy", "repository"]);
    let summary = serde_json::json!({
        "session_id": session_id,
        "repository_id": repository_id,
        "tasks": [],
        "json_path": "/data/sessions/legacy.json"
    });
    let summary: SessionSummary =
        serde_json::from_value(summary).expect("legacy summary should deserialize");
    assert_eq!(summary.created_at_unix_ms, 0);
    assert_eq!(summary.updated_at_unix_ms, 0);

    let context = serde_json::json!({
        "schema_version": 1,
        "session_id": session_id,
        "repository_id": repository_id,
        "tasks": [],
        "usage": {
            "tokens": {
                "input_tokens": 0,
                "output_tokens": 0,
                "cached_input_tokens": 0,
                "total_tokens": 0
            },
            "cost": null
        },
        "json_path": "/data/sessions/legacy.json"
    });
    let context: SessionContext =
        serde_json::from_value(context).expect("legacy context should deserialize");
    assert_eq!(context.created_at_unix_ms, 0);
    assert_eq!(context.updated_at_unix_ms, 0);
}
