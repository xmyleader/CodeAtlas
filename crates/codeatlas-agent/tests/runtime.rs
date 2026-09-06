#![allow(clippy::expect_used, clippy::too_many_lines)]

use std::{
    collections::VecDeque,
    future::pending,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use codeatlas_agent::{
    AgentRequest, AgentRuntime, AssistantOutput, AssistantToolCall, CancellationToken,
    ChannelEventSink, ConversationMessage, ConversationRole, MockModelClient, ModelClient,
    ModelError, ModelMessage, ModelPricing, ModelRequest, ModelResponse, RepositoryToolEnvelope,
    RuntimeConfig, RuntimeError, SUBMIT_ANSWER_TOOL_NAME, SYSTEM_PROMPT, StructuredAnswer,
    StructuredCallPath, StructuredClaim, StructuredDiagram, StructuredDiagramDecision,
    StructuredDiagramEdge, StructuredDiagramNode,
};
use codeatlas_core::{
    AppEvent, CallPathStep, ClaimKind, DiagramDecision, DiagramKind, Evidence, EvidenceId,
    EvidenceValidationError, ExplanationAudience, ExplanationDepth, ExplanationProfile, FileId,
    ModelBudget, ModelCallOutcome, MonetaryBudget, ProgressPhase, RepositoryId, RepositoryPath,
    RequestId, SessionId, SourceSpan, SuggestedAction, SymbolId, TargetResolution, TokenUsage,
    ToolCall, ToolDefinition, ToolError, ToolExecutor, ToolOutput, WorkflowEvent,
};
use serde_json::{Value, json};
use tokio::sync::oneshot;

enum ToolStep {
    Success {
        data: Value,
        evidence: Vec<Evidence>,
    },
    Failure(ToolError),
}

#[derive(Default)]
struct ScriptedExecutor {
    script: Mutex<VecDeque<ToolStep>>,
    calls: Mutex<Vec<ToolCall>>,
}

impl ScriptedExecutor {
    fn new(script: impl IntoIterator<Item = ToolStep>) -> Self {
        Self {
            script: Mutex::new(script.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<ToolCall> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait]
impl ToolExecutor for ScriptedExecutor {
    async fn execute(&self, call: &ToolCall) -> Result<ToolOutput, ToolError> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call.clone());
        let step = self
            .script
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_front()
            .expect("test executor script should have a step");
        match step {
            ToolStep::Success { data, evidence } => Ok(ToolOutput {
                call_id: call.id,
                result: serde_json::to_value(RepositoryToolEnvelope { data, evidence })
                    .expect("repository envelope should serialize"),
                is_error: false,
            }),
            ToolStep::Failure(error) => Err(error),
        }
    }
}

fn tool(name: &str) -> ToolDefinition {
    ToolDefinition {
        name: name.to_owned(),
        description: format!("read-only {name}"),
        input_schema: json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
        output_schema: None,
    }
}

fn request(label: &str) -> AgentRequest {
    AgentRequest::new(
        RequestId::from_stable_parts(&[label, "request"]),
        SessionId::from_stable_parts(&[label, "session"]),
        RepositoryId::from_stable_parts(&[label, "repository"]),
        "How does this repository work?",
    )
}

struct WindowedModel {
    inner: MockModelClient,
    context_window_tokens: u32,
}

impl WindowedModel {
    fn new(
        context_window_tokens: u32,
        script: impl IntoIterator<Item = Result<ModelResponse, ModelError>>,
    ) -> Self {
        Self {
            inner: MockModelClient::new(script),
            context_window_tokens,
        }
    }

    fn requests(&self) -> Vec<ModelRequest> {
        self.inner.requests()
    }
}

#[async_trait]
impl ModelClient for WindowedModel {
    fn context_window_tokens(&self) -> Option<u32> {
        Some(self.context_window_tokens)
    }

    async fn complete(&self, request: ModelRequest) -> Result<ModelResponse, ModelError> {
        self.inner.complete(request).await
    }
}

fn evidence(label: &str) -> Evidence {
    Evidence {
        id: EvidenceId::from_stable_parts(&[label]),
        file_id: FileId::from_stable_parts(&["src/lib.rs"]),
        path: RepositoryPath::new("src/lib.rs").expect("valid repository path"),
        span: SourceSpan::new(1, 0, 2, 0).expect("valid source span"),
        symbol_id: None,
        excerpt: Some("pub fn run() {}".to_owned()),
    }
}

fn usage(total: u64) -> TokenUsage {
    TokenUsage {
        input_tokens: total,
        output_tokens: 0,
        cached_input_tokens: 0,
        total_tokens: total,
    }
}

fn tool_response(id: &str, name: &str, total_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: AssistantOutput::ToolCalls {
            content: None,
            calls: vec![AssistantToolCall {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments: json!({}),
            }],
        },
        usage: Some(usage(total_tokens)),
        finish_reason: Some("tool_calls".to_owned()),
    }
}

fn final_response(claims: Vec<StructuredClaim>, total_tokens: u64) -> ModelResponse {
    final_response_with_diagram(claims, not_needed_diagram(), total_tokens)
}

fn final_response_with_diagram(
    claims: Vec<StructuredClaim>,
    diagram: StructuredDiagramDecision,
    total_tokens: u64,
) -> ModelResponse {
    ModelResponse {
        output: AssistantOutput::FinalAnswer {
            answer: StructuredAnswer {
                text: "A grounded answer".to_owned(),
                claims,
                call_paths: Vec::new(),
                diagram,
            },
        },
        usage: Some(usage(total_tokens)),
        finish_reason: Some("stop".to_owned()),
    }
}

fn unstructured_response(content: &str, total_tokens: u64) -> ModelResponse {
    ModelResponse {
        output: AssistantOutput::UnstructuredText {
            content: content.to_owned(),
        },
        usage: Some(usage(total_tokens)),
        finish_reason: Some("stop".to_owned()),
    }
}

fn submitted_answer_response(
    provider_call_id: &str,
    claims: Vec<StructuredClaim>,
    total_tokens: u64,
) -> ModelResponse {
    submitted_answer_arguments_response(
        provider_call_id,
        serde_json::to_value(StructuredAnswer {
            text: "A grounded answer".to_owned(),
            claims,
            call_paths: Vec::new(),
            diagram: not_needed_diagram(),
        })
        .expect("structured answer should serialize"),
        total_tokens,
    )
}

fn submitted_answer_arguments_response(
    provider_call_id: &str,
    arguments: Value,
    total_tokens: u64,
) -> ModelResponse {
    ModelResponse {
        output: AssistantOutput::ToolCalls {
            content: None,
            calls: vec![AssistantToolCall {
                id: provider_call_id.to_owned(),
                name: SUBMIT_ANSWER_TOOL_NAME.to_owned(),
                arguments,
            }],
        },
        usage: Some(usage(total_tokens)),
        finish_reason: Some("tool_calls".to_owned()),
    }
}

fn submitted_answer_with_call_paths(
    provider_call_id: &str,
    claims: Vec<StructuredClaim>,
    call_paths: Vec<StructuredCallPath>,
    total_tokens: u64,
) -> ModelResponse {
    submitted_answer_arguments_response(
        provider_call_id,
        serde_json::to_value(StructuredAnswer {
            text: "A grounded answer".to_owned(),
            claims,
            call_paths,
            diagram: not_needed_diagram(),
        })
        .expect("structured answer should serialize"),
        total_tokens,
    )
}

fn not_needed_diagram() -> StructuredDiagramDecision {
    StructuredDiagramDecision::NotNeeded {
        reason: "A diagram would not add explanatory value.".to_owned(),
    }
}

fn needed_diagram(kind: DiagramKind, _evidence_id: EvidenceId) -> StructuredDiagramDecision {
    let node = |id: &str, label: &str| StructuredDiagramNode {
        id: id.to_owned(),
        label: label.to_owned(),
        claim_indices: vec![0],
    };
    StructuredDiagramDecision::Needed {
        reason: "The topology is materially clearer as a diagram.".to_owned(),
        diagram: StructuredDiagram {
            kind,
            title: "Runtime topology".to_owned(),
            nodes: vec![node("runtime", "Runtime"), node("service", "Service")],
            edges: vec![StructuredDiagramEdge {
                source: "runtime".to_owned(),
                target: "service".to_owned(),
                label: "delegates".to_owned(),
                claim_indices: vec![0],
            }],
        },
    }
}

fn assert_object_schemas_are_closed(schema: &Value) {
    if schema.get("type").and_then(Value::as_str) == Some("object") {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&Value::Bool(false)),
            "object schema must reject additional properties: {schema}"
        );
    }
    match schema {
        Value::Array(values) => {
            for value in values {
                assert_object_schemas_are_closed(value);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                assert_object_schemas_are_closed(value);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn model_tool_message<'a>(request: &'a ModelRequest, provider_call_id: &str) -> &'a ModelMessage {
    request
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some(provider_call_id))
        .expect("model request should contain the expected tool response")
}

fn assert_tool_protocol(request: &ModelRequest, expected_provider_ids: &[&str]) {
    let assistant_ids = request
        .messages
        .iter()
        .flat_map(|message| message.tool_calls.iter())
        .map(|call| call.id.as_str())
        .collect::<Vec<_>>();
    let tool_ids = request
        .messages
        .iter()
        .filter_map(|message| message.tool_call_id.as_deref())
        .collect::<Vec<_>>();

    assert_eq!(assistant_ids, expected_provider_ids);
    assert_eq!(tool_ids, expected_provider_ids);
}

#[test]
fn agent_request_serde_defaults_the_profile_and_constructor_helpers_preserve_it() {
    let original = request("request-serde");
    let mut legacy = serde_json::to_value(&original).expect("request should serialize");
    legacy
        .as_object_mut()
        .expect("request should be an object")
        .remove("profile");
    let restored: AgentRequest =
        serde_json::from_value(legacy).expect("legacy request should deserialize");
    assert_eq!(restored.profile, ExplanationProfile::default());

    let profile = ExplanationProfile::new(ExplanationAudience::Expert, ExplanationDepth::Detail);
    assert_eq!(
        original
            .with_history(vec![ConversationMessage {
                role: ConversationRole::User,
                content: "prior".to_owned(),
            }])
            .with_profile(profile)
            .profile,
        profile
    );
}

#[tokio::test]
async fn profile_reaches_the_exact_model_request_and_observable_transcript() {
    let model = Arc::new(MockModelClient::new([Ok(final_response(Vec::new(), 1))]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let profile =
        ExplanationProfile::new(ExplanationAudience::Beginner, ExplanationDepth::Workflow);
    let request = request("profile-request").with_profile(profile);
    let request_id = request.request_id;
    let (sink, mut events) = ChannelEventSink::channel();

    runtime
        .run(request, CancellationToken::new(), &sink)
        .await
        .expect("profile request should complete");

    let model_request = &model.requests()[0];
    assert_eq!(
        model_request.messages[0],
        ModelMessage::system(SYSTEM_PROMPT)
    );
    assert_eq!(
        model_request.messages[1],
        ModelMessage::system(profile.control_message())
    );
    assert_eq!(
        model_request.messages[2],
        ModelMessage::user("How does this repository work?")
    );
    let expected_control = serde_json::to_value(ModelMessage::system(profile.control_message()))
        .expect("control message should serialize");
    assert!(
        std::iter::from_fn(|| events.try_recv().ok()).any(|event| matches!(
            event,
            AppEvent::TaskTraceRecorded {
                request_id: event_request_id,
                event: WorkflowEvent::Message(value),
            } if event_request_id == request_id && value == expected_control
        ))
    );
}

#[tokio::test]
async fn suggested_actions_are_deterministic_bounded_and_reference_final_answer() {
    let item = evidence("suggested-actions");
    let path = StructuredCallPath {
        label: Some("runtime dispatch".to_owned()),
        steps: vec![CallPathStep {
            target: TargetResolution::Resolved(SymbolId::from_stable_parts(&[
                "suggested-actions-target",
            ])),
            call_edge_id: None,
            evidence_ids: vec![item.id],
        }],
        complete: false,
    };
    let mut answers = Vec::new();
    for _ in 0..2 {
        let model = Arc::new(MockModelClient::new([
            Ok(tool_response("suggested-tool", "inspect", 1)),
            Ok(submitted_answer_with_call_paths(
                "suggested-final",
                vec![StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "The runtime dispatches the request.".to_owned(),
                    evidence_ids: vec![item.id],
                }],
                vec![path.clone()],
                1,
            )),
        ]));
        let runtime = AgentRuntime::new(
            model,
            Arc::new(ScriptedExecutor::new([ToolStep::Success {
                data: json!({"symbol": "dispatch"}),
                evidence: vec![item.clone()],
            }])),
            vec![tool("inspect")],
            RuntimeConfig::default(),
        )
        .expect("valid runtime");
        answers.push(
            runtime
                .run_silent(request("suggested-actions"), CancellationToken::new())
                .await
                .expect("grounded answer should complete"),
        );
    }

    assert_eq!(answers[0].suggested_actions, answers[1].suggested_actions);
    assert_eq!(answers[0].suggested_actions.len(), 4);
    assert_eq!(
        answers[0].suggested_actions,
        vec![
            SuggestedAction::DeepenClaim {
                claim_id: answers[0].claims[0].id,
            },
            SuggestedAction::ContinueCallPath {
                call_path_id: answers[0].call_paths[0].id,
            },
            SuggestedAction::ShowSource {
                evidence_id: answers[0].evidence[0].id,
            },
            SuggestedAction::ChangeDepth {
                depth: ExplanationDepth::Architecture,
            },
        ]
    );
    answers[0]
        .validate_evidence()
        .expect("every generated reference must exist in the pruned answer");
}

#[tokio::test]
async fn default_configuration_reports_usage_without_enabling_a_budget() {
    let model = Arc::new(MockModelClient::new([Ok(final_response(
        vec![StructuredClaim {
            kind: ClaimKind::Inference,
            text: "A large request can still complete".to_owned(),
            evidence_ids: Vec::new(),
        }],
        1_000_000,
    ))]));
    let config = RuntimeConfig::default();
    assert!(!config.budget.is_enabled());
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        config,
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("unbounded-tokens"), CancellationToken::new())
        .await
        .expect("large reported token usage must not stop the agent");

    assert_eq!(
        answer
            .usage
            .expect("successful answers include usage")
            .tokens
            .total_tokens,
        1_000_000
    );
}

#[tokio::test]
async fn omitted_usage_stays_absent_from_events_and_final_answer() {
    let mut response = final_response(Vec::new(), 0);
    response.usage = None;
    let runtime = AgentRuntime::new(
        Arc::new(MockModelClient::new([Ok(response)])),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let agent_request = request("omitted-usage");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(agent_request, CancellationToken::new(), &sink)
        .await
        .expect("answer without provider usage should complete");
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }

    assert_eq!(answer.usage, None);
    assert!(
        events
            .iter()
            .all(|event| !matches!(event, AppEvent::UsageUpdated { .. }))
    );
}

#[tokio::test]
async fn explicitly_reported_zero_usage_remains_present() {
    let runtime = AgentRuntime::new(
        Arc::new(MockModelClient::new([Ok(final_response(Vec::new(), 0))])),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let agent_request = request("reported-zero-usage");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(agent_request, CancellationToken::new(), &sink)
        .await
        .expect("answer with reported zero usage should complete");
    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }

    assert_eq!(
        answer.usage.as_ref().map(|usage| usage.tokens),
        Some(TokenUsage::default())
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AppEvent::UsageUpdated { .. }))
            .count(),
        1
    );
}

#[tokio::test]
async fn model_call_ledger_records_retries_with_stable_sequence_and_outcome() {
    let model = Arc::new(MockModelClient::new([
        Err(ModelError::Transport {
            message: "temporary disconnect".to_owned(),
        }),
        Ok(final_response(Vec::new(), 7)),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            max_model_retries: 1,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");
    let request = request("model-ledger");
    let request_id = request.request_id;
    let (sink, mut receiver) = ChannelEventSink::channel();

    runtime
        .run(request, CancellationToken::new(), &sink)
        .await
        .expect("retry should recover");
    let mut records = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let AppEvent::ModelCallRecorded { record, .. } = event {
            records.push(record);
        }
    }

    assert_eq!(records.len(), 2);
    assert_eq!(records[0].sequence, 1);
    assert_eq!(records[1].sequence, 2);
    assert_ne!(records[0].id, records[1].id);
    assert_eq!(
        records[0].id,
        codeatlas_core::ModelCallId::from_stable_parts(&[&request_id.to_string(), "1"])
    );
    assert!(matches!(
        records[0].outcome,
        ModelCallOutcome::Failed {
            retryable: true,
            ..
        }
    ));
    assert!(matches!(records[1].outcome, ModelCallOutcome::Succeeded));
    assert_eq!(records[1].usage.map(|usage| usage.total_tokens), Some(7));
}

#[tokio::test]
async fn retryable_model_timeout_is_ledgered_as_timed_out_before_retrying() {
    let model = Arc::new(MockModelClient::new([
        Err(ModelError::Timeout { timeout_ms: 25 }),
        Ok(final_response(Vec::new(), 7)),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            max_model_retries: 1,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    runtime
        .run(request("timeout-ledger"), CancellationToken::new(), &sink)
        .await
        .expect("retry should recover");
    let records = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::ModelCallRecorded { record, .. } => Some(record),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(records.len(), 2);
    assert!(matches!(records[0].outcome, ModelCallOutcome::TimedOut));
    assert!(matches!(records[1].outcome, ModelCallOutcome::Succeeded));
}

#[tokio::test]
async fn token_budget_stops_before_any_follow_up_model_request() {
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("budget-tool", "read_source", 10)),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([ToolStep::Success {
        data: json!({}),
        evidence: Vec::new(),
    }]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        vec![tool("read_source")],
        RuntimeConfig {
            budget: ModelBudget {
                max_total_tokens: Some(10),
                max_cost: None,
            },
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");
    let request = request("token-budget");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let error = runtime
        .run(request, CancellationToken::new(), &sink)
        .await
        .expect_err("budget must terminate the task");
    let events = std::iter::from_fn(|| receiver.try_recv().ok()).collect::<Vec<_>>();

    assert!(matches!(
        error,
        RuntimeError::TokenBudgetExceeded {
            used: 10,
            limit: 10
        }
    ));
    assert_eq!(model.requests().len(), 1);
    assert!(executor.calls().is_empty());
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::BudgetExceeded {
            reason: codeatlas_core::BudgetStopReason::TotalTokensReached {
                used: 10,
                limit: 10
            },
            ..
        }
    )));
}

#[tokio::test]
async fn enabled_budget_fails_closed_when_provider_omits_usage() {
    let mut response = final_response(Vec::new(), 1);
    response.usage = None;
    let model = Arc::new(MockModelClient::new([Ok(response)]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            budget: ModelBudget {
                max_total_tokens: Some(100),
                max_cost: None,
            },
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(request("missing-budget-usage"), CancellationToken::new())
        .await
        .expect_err("missing usage makes the budget unenforceable");

    assert!(matches!(error, RuntimeError::BudgetUnenforceable));
    assert_eq!(error.to_app_error().code, "budget_unenforceable");
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn monetary_budget_uses_explicit_per_call_pricing() {
    let model = Arc::new(MockModelClient::new([Ok(final_response(
        Vec::new(),
        1_000_000,
    ))]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            pricing: Some(ModelPricing {
                currency: "USD".to_owned(),
                input_per_million: 1.0,
                cached_input_per_million: Some(0.2),
                output_per_million: 4.0,
            }),
            budget: ModelBudget {
                max_total_tokens: None,
                max_cost: Some(MonetaryBudget {
                    currency: "USD".to_owned(),
                    amount: 0.5,
                }),
            },
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let error = runtime
        .run(request("cost-budget"), CancellationToken::new(), &sink)
        .await
        .expect_err("estimated cost reaches the configured limit");
    let records = std::iter::from_fn(|| receiver.try_recv().ok())
        .filter_map(|event| match event {
            AppEvent::ModelCallRecorded { record, .. } => Some(record),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert!(matches!(error, RuntimeError::CostBudgetExceeded { .. }));
    assert_eq!(model.requests().len(), 1);
    assert_eq!(records[0].cost.as_ref().map(|cost| cost.amount), Some(1.0));
}

#[test]
fn system_prompt_uses_semantic_diagram_criteria_without_size_thresholds() {
    assert!(SYSTEM_PROMPT.contains("architecture boundaries or responsibility topology"));
    assert!(SYSTEM_PROMPT.contains("non-trivial flow with branches or states"));
    assert!(SYSTEM_PROMPT.contains("entity relationships"));
    assert!(SYSTEM_PROMPT.contains("single symbol"));
    assert!(SYSTEM_PROMPT.contains("every node and edge"));
    assert!(SYSTEM_PROMPT.contains("call_paths"));
    assert!(
        SYSTEM_PROMPT
            .contains("Every call_paths step must cite only evidence IDs returned by tools")
    );
    assert!(SYSTEM_PROMPT.contains("Match exploration depth and answer length"));
    assert!(SYSTEM_PROMPT.contains("brief introduction or overview"));
    assert!(SYSTEM_PROMPT.contains("minimum sufficient evidence supports the answer"));
    assert!(SYSTEM_PROMPT.contains("direct user request to draw, show, or produce a diagram"));
    assert!(SYSTEM_PROMPT.contains("CodeAtlas derives element evidence from those claims"));
    assert!(SYSTEM_PROMPT.contains("ASCII art"));
    assert!(SYSTEM_PROMPT.contains(
        "Never use file count, line count, repository size, answer length, or any quantitative threshold"
    ));
    assert!(!SYSTEM_PROMPT.contains("more than 10 files"));
    assert!(!SYSTEM_PROMPT.contains("more than 100 lines"));
}

#[tokio::test]
async fn submit_answer_schema_requires_an_explicit_strict_diagram_decision() {
    let model = Arc::new(MockModelClient::new([Ok(final_response(Vec::new(), 1))]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    runtime
        .run_silent(request("diagram-schema"), CancellationToken::new())
        .await
        .expect("not-needed answer should be valid");

    let requests = model.requests();
    let schema = &requests[0]
        .tools
        .iter()
        .find(|tool| tool.name == SUBMIT_ANSWER_TOOL_NAME)
        .expect("submit_answer tool")
        .input_schema;
    assert_eq!(
        schema["required"],
        json!(["text", "claims", "call_paths", "diagram"])
    );
    assert!(
        schema["properties"]["call_paths"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("returned by repository tools"))
    );
    assert!(schema["properties"]["call_paths"]["items"]["properties"]["steps"]["items"]
        ["properties"]["evidence_ids"]["description"]
        .as_str()
        .is_some_and(|description| description.contains("returned by repository tools")));
    let variants = schema["properties"]["diagram"]["oneOf"]
        .as_array()
        .expect("diagram decision variants");
    assert_eq!(variants.len(), 2);
    let not_needed = variants
        .iter()
        .find(|variant| variant["properties"]["decision"]["enum"] == json!(["not_needed"]))
        .expect("not-needed variant");
    assert_eq!(not_needed["required"], json!(["decision", "reason"]));
    let needed = variants
        .iter()
        .find(|variant| variant["properties"]["decision"]["enum"] == json!(["needed"]))
        .expect("needed variant");
    assert_eq!(needed["required"], json!(["decision", "reason", "diagram"]));
    let diagram = &needed["properties"]["diagram"];
    assert_eq!(
        diagram["properties"]["kind"]["enum"],
        json!(["architecture", "flow", "relationship"])
    );
    assert_eq!(diagram["properties"]["nodes"]["minItems"], 2);
    assert_eq!(diagram["properties"]["nodes"]["maxItems"], 32);
    assert_eq!(diagram["properties"]["edges"]["minItems"], 1);
    assert_eq!(diagram["properties"]["edges"]["maxItems"], 64);
    for element in [
        &diagram["properties"]["nodes"]["items"],
        &diagram["properties"]["edges"]["items"],
    ] {
        let claim_indices = &element["properties"]["claim_indices"];
        assert_eq!(claim_indices["minItems"], 1);
        assert_eq!(claim_indices["uniqueItems"], true);
        assert_eq!(claim_indices["items"]["type"], "integer");
        assert_eq!(claim_indices["items"]["minimum"], 0);
        assert!(element["properties"].get("evidence_ids").is_none());
        assert!(
            element["required"]
                .as_array()
                .is_some_and(|required| !required.contains(&json!("evidence_ids")))
        );
    }
    assert_object_schemas_are_closed(schema);
}

#[tokio::test]
async fn multi_round_loop_deduplicates_evidence_and_emits_core_events() {
    let item = evidence("run-function");
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-1", "read_source", 10)),
        Ok(tool_response("provider-2", "trace_calls", 11)),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "run is defined in src/lib.rs".to_owned(),
                evidence_ids: vec![item.id],
            }],
            12,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([
        ToolStep::Success {
            data: json!({"source": "pub fn run() {}"}),
            evidence: vec![item.clone()],
        },
        ToolStep::Success {
            data: json!({"callees": []}),
            evidence: vec![item.clone()],
        },
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        vec![tool("read_source"), tool("trace_calls")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let agent_request = request("multi-round");
    let request_id = agent_request.request_id;
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(agent_request, CancellationToken::new(), &sink)
        .await
        .expect("agent loop should complete");

    assert_eq!(answer.evidence, vec![item]);
    assert_eq!(
        answer.usage.as_ref().expect("usage").tokens.total_tokens,
        33
    );
    assert_eq!(executor.calls().len(), 2);
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[0].messages[0]
            .content
            .as_deref()
            .expect("system prompt")
            .contains("UNTRUSTED DATA")
    );
    assert_eq!(
        requests[1]
            .messages
            .iter()
            .filter(|message| message.tool_call_id.is_some())
            .count(),
        1
    );

    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AppEvent::EvidenceAdded { .. }))
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter_map(|event| match event {
                AppEvent::UsageUpdated { usage, .. } => Some(usage.tokens.total_tokens),
                _ => None,
            })
            .collect::<Vec<_>>(),
        vec![10, 21, 33]
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AppEvent::ToolCallStarted { .. }))
            .count(),
        2
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, AppEvent::ToolCallCompleted { .. }))
            .count(),
        2
    );
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Progress { progress, .. } if progress.phase == ProgressPhase::Reading
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Progress { progress, .. } if progress.phase == ProgressPhase::Tracing
    )));
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::AnswerCompleted { request_id: id, .. } if *id == request_id
    )));
}

#[tokio::test]
async fn final_answer_retains_only_evidence_referenced_by_its_contract() {
    let explored = evidence("explored-but-unused");
    let cited = evidence("cited-by-final-claim");
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-unused", "read_source", 1)),
        Ok(tool_response("provider-cited", "read_source", 1)),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "The cited source supports the answer".to_owned(),
                evidence_ids: vec![cited.id],
            }],
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::new([
            ToolStep::Success {
                data: json!({"source": "exploration"}),
                evidence: vec![explored],
            },
            ToolStep::Success {
                data: json!({"source": "cited"}),
                evidence: vec![cited.clone()],
            },
        ])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("prune-unused-evidence"), CancellationToken::new())
        .await
        .expect("referenced evidence should produce a valid answer");

    assert_eq!(answer.evidence, vec![cited]);
}

#[tokio::test]
async fn model_context_retains_source_and_semantic_json_fields() {
    let source = "pub fn retained_source() { /* evidence */ }\n".repeat(700);
    let mut item = evidence("retained-source");
    item.symbol_id = Some(SymbolId::from_stable_parts(&["retained_source"]));
    item.excerpt = Some(source.clone());
    let first_data = json!({
        "path": "src/lib.rs",
        "file_id": item.file_id,
        "content": source,
        "truncated": false,
        "optional": null
    });
    let repeated_data = json!({
        "symbol": {
            "name": "retained_source",
            "path": "src/lib.rs",
            "file_id": item.file_id,
            "optional": null
        },
        "cached": false
    });
    let canonical_first = serde_json::to_value(RepositoryToolEnvelope {
        data: first_data.clone(),
        evidence: vec![item.clone()],
    })
    .expect("canonical repository envelope should serialize");
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-read", "read_file", 10)),
        Ok(tool_response("provider-repeat", "find_symbol", 11)),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "retained_source is defined in src/lib.rs".to_owned(),
                evidence_ids: vec![item.id],
            }],
            12,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([
        ToolStep::Success {
            data: first_data,
            evidence: vec![item.clone()],
        },
        ToolStep::Success {
            data: repeated_data,
            evidence: vec![item.clone()],
        },
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor,
        vec![tool("read_file"), tool("find_symbol")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(
            request("context-compaction"),
            CancellationToken::new(),
            &sink,
        )
        .await
        .expect("compacted model context should still produce a grounded answer");

    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert_tool_protocol(&requests[1], &["provider-read"]);
    assert_tool_protocol(&requests[2], &["provider-read", "provider-repeat"]);
    assert_eq!(requests[0].tools, requests[1].tools);
    assert_eq!(requests[1].tools, requests[2].tools);
    assert_eq!(requests[0].messages[0], requests[1].messages[0]);
    assert_eq!(requests[1].messages[0], requests[2].messages[0]);
    assert!(
        !requests[2].messages[0]
            .content
            .as_deref()
            .expect("system prompt should have content")
            .contains("/* evidence */")
    );

    let fresh: Value = serde_json::from_str(
        model_tool_message(&requests[1], "provider-read")
            .content
            .as_deref()
            .expect("fresh tool response should have content"),
    )
    .expect("fresh tool content should be JSON");
    assert_eq!(fresh["data"]["content"].as_str(), item.excerpt.as_deref());
    assert_eq!(fresh["data"]["file_id"], json!(item.file_id));
    assert_eq!(fresh["data"]["truncated"], json!(false));
    assert_eq!(fresh["data"]["optional"], Value::Null);
    assert_eq!(fresh["evidence"][0]["id"], json!(item.id));
    assert_eq!(fresh["evidence"][0]["path"], json!(item.path.as_str()));
    assert_eq!(fresh["evidence"][0]["span"], json!(item.span));
    assert_eq!(fresh["evidence"][0]["symbol_id"], json!(item.symbol_id));
    assert!(fresh["evidence"][0].get("file_id").is_none());
    assert!(fresh["evidence"][0].get("excerpt").is_none());

    let retained_content = model_tool_message(&requests[2], "provider-read")
        .content
        .as_deref()
        .expect("retained tool response should have content");
    let retained: Value =
        serde_json::from_str(retained_content).expect("retained tool content should be JSON");
    let retained_source = retained["data"]["content"]
        .as_str()
        .expect("retained data should preserve source text");
    assert_eq!(retained_source, item.excerpt.as_deref().expect("excerpt"));
    assert_eq!(retained["data"]["truncated"], json!(false));
    assert_eq!(retained["data"]["optional"], Value::Null);
    assert_eq!(retained["evidence"][0]["id"], json!(item.id));
    assert_eq!(retained["evidence"][0]["path"], json!(item.path.as_str()));
    assert_eq!(retained["evidence"][0]["span"], json!(item.span));
    assert!(retained["evidence"][0].get("excerpt").is_none());

    let repeated: Value = serde_json::from_str(
        model_tool_message(&requests[2], "provider-repeat")
            .content
            .as_deref()
            .expect("new repeated tool response should have content"),
    )
    .expect("repeated tool content should be JSON");
    assert_eq!(repeated["evidence"], json!([{"id": item.id}]));
    assert_eq!(repeated["data"]["symbol"]["name"], json!("retained_source"));
    assert_eq!(repeated["data"]["symbol"]["file_id"], json!(item.file_id));
    assert_eq!(repeated["data"]["symbol"]["optional"], Value::Null);
    assert_eq!(repeated["data"]["cached"], json!(false));

    let mut completed_outputs = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        if let AppEvent::ToolCallCompleted { output, .. } = event {
            completed_outputs.push(output);
        }
    }
    assert_eq!(completed_outputs.len(), 2);
    assert_eq!(completed_outputs[0].result, canonical_first);
    assert_eq!(answer.evidence, vec![item]);
}

#[tokio::test]
async fn known_context_window_drops_oldest_history_before_sending() {
    let model = Arc::new(WindowedModel::new(
        8_192,
        [Ok(final_response(Vec::new(), 1))],
    ));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let long = "old conversation detail ".repeat(1_000);
    let request = request("bounded-history").with_history(vec![
        ConversationMessage {
            role: ConversationRole::User,
            content: long.clone(),
        },
        ConversationMessage {
            role: ConversationRole::Assistant,
            content: long.clone(),
        },
        ConversationMessage {
            role: ConversationRole::User,
            content: long.clone(),
        },
        ConversationMessage {
            role: ConversationRole::Assistant,
            content: long,
        },
    ]);

    runtime
        .run_silent(request, CancellationToken::new())
        .await
        .expect("old history should be reduced instead of overflowing context");

    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].messages.len(), 3);
    let system = requests[0].messages[0]
        .content
        .as_deref()
        .expect("system prompt");
    assert!(system.starts_with(SYSTEM_PROMPT));
    assert!(system.contains("oldest conversation turns were omitted"));
    assert_eq!(
        requests[0].messages[2].content.as_deref(),
        Some("How does this repository work?")
    );
}

#[tokio::test]
async fn known_context_window_removes_complete_old_tool_turns_without_orphans() {
    let model = Arc::new(WindowedModel::new(
        16_384,
        [Ok(final_response(Vec::new(), 1))],
    ));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let large_result = |label: &str| {
        json!({
            "data": {"content": format!("{label}:{}", "x".repeat(20_000))},
            "evidence": []
        })
        .to_string()
    };
    let history = vec![
        ModelMessage::user("old tool question"),
        ModelMessage::assistant_tool_calls(
            None,
            vec![AssistantToolCall {
                id: "old-history-tool".to_owned(),
                name: "read_source".to_owned(),
                arguments: json!({"path": "old.rs"}),
            }],
        ),
        ModelMessage::tool(
            "old-history-tool",
            "read_source",
            large_result("old-result"),
        ),
        ModelMessage::assistant("old tool answer"),
        ModelMessage::user("newer tool question"),
        ModelMessage::assistant_tool_calls(
            None,
            vec![AssistantToolCall {
                id: "new-history-tool".to_owned(),
                name: "read_source".to_owned(),
                arguments: json!({"path": "new.rs"}),
            }],
        ),
        ModelMessage::tool(
            "new-history-tool",
            "read_source",
            large_result("new-result"),
        ),
        ModelMessage::assistant("newer tool answer"),
    ];

    runtime
        .run_silent(
            request("bounded-tool-history").with_transcript(history),
            CancellationToken::new(),
        )
        .await
        .expect("complete historical tool groups should compact safely");

    let sent = &model.requests()[0];
    assert!(sent.messages.iter().all(|message| {
        message.tool_call_id.as_deref() != Some("old-history-tool")
            && message
                .tool_calls
                .iter()
                .all(|call| call.id != "old-history-tool")
    }));
    assert_tool_protocol(sent, &["new-history-tool"]);
    assert_eq!(
        sent.messages
            .last()
            .and_then(|message| message.content.as_deref()),
        Some("How does this repository work?")
    );
}

#[tokio::test]
async fn context_overflow_retry_compacts_payload_instead_of_repeating_it() {
    let source = "pub fn large_source() {}\n".repeat(2_000);
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("large-read", "read_source", 1)),
        Err(ModelError::Http {
            status: 413,
            message: "maximum context length exceeded".to_owned(),
            retryable: true,
        }),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"content": source}),
            evidence: Vec::new(),
        }])),
        vec![tool("read_source")],
        RuntimeConfig {
            max_model_retries: 1,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    runtime
        .run_silent(request("overflow-retry"), CancellationToken::new())
        .await
        .expect("context overflow should recover with a compacted retry");

    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let full = model_tool_message(&requests[1], "large-read")
        .content
        .as_deref()
        .expect("full tool result");
    let compacted = model_tool_message(&requests[2], "large-read")
        .content
        .as_deref()
        .expect("compacted tool result");
    assert!(compacted.len() < full.len());
    assert!(compacted.contains("context_compacted"));
    assert!(!compacted.contains("pub fn large_source"));
    assert_tool_protocol(&requests[2], &["large-read"]);
}

#[tokio::test]
async fn submit_answer_tool_finishes_without_executor_dispatch() {
    let model = Arc::new(MockModelClient::new([Ok(submitted_answer_response(
        "provider-submit",
        vec![StructuredClaim {
            kind: ClaimKind::Inference,
            text: "A grounded answer".to_owned(),
            evidence_ids: Vec::new(),
        }],
        4,
    ))]));
    let executor = Arc::new(ScriptedExecutor::default());
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("submit-answer"), CancellationToken::new())
        .await
        .expect("submit_answer should finish the request");

    assert_eq!(answer.text, "A grounded answer");
    assert!(matches!(
        answer.diagram,
        DiagramDecision::NotNeeded { ref reason }
            if reason == "A diagram would not add explanatory value."
    ));
    assert!(executor.calls().is_empty());
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    assert!(
        requests[0]
            .tools
            .iter()
            .any(|tool| tool.name == SUBMIT_ANSWER_TOOL_NAME)
    );
}

#[tokio::test]
async fn invalid_submit_answer_arguments_get_one_repair_turn() {
    let invalid_arguments = json!({
        "claims": [],
        "call_paths": [],
        "diagram": {
            "decision": "not_needed",
            "reason": "A direct answer does not need a diagram."
        }
    });
    let model = Arc::new(MockModelClient::new([
        Ok(submitted_answer_arguments_response(
            "provider-invalid-submit",
            invalid_arguments,
            2,
        )),
        Ok(submitted_answer_response(
            "provider-repaired-submit",
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "A grounded answer".to_owned(),
                evidence_ids: Vec::new(),
            }],
            3,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::default());
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(
            request("invalid-submit-repair"),
            CancellationToken::new(),
            &sink,
        )
        .await
        .expect("one invalid submit_answer repair should recover");

    assert_eq!(answer.text, "A grounded answer");
    assert!(executor.calls().is_empty());
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_tool_protocol(&requests[1], &["provider-invalid-submit"]);
    let repair_result = model_tool_message(&requests[1], "provider-invalid-submit")
        .content
        .as_deref()
        .expect("invalid submit_answer should receive a tool result");
    let repair_result: Value =
        serde_json::from_str(repair_result).expect("repair result should be JSON");
    assert_eq!(repair_result["error"]["code"], "invalid_submit_answer");
    assert!(
        repair_result["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("missing field `text`"))
    );
    assert!(requests[1].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("top-level text field is mandatory")
                && content.contains("Validation error: missing field `text`")
        })
    }));
    let mut repair_message = None;
    while let Ok(event) = receiver.try_recv() {
        if let AppEvent::Progress { progress, .. } = event
            && progress
                .message
                .contains("requesting structured answer repair")
        {
            repair_message = Some(progress.message);
        }
    }
    assert!(
        repair_message
            .as_deref()
            .is_some_and(|message| message.contains("missing field `text`"))
    );
}

#[tokio::test]
async fn repeatedly_invalid_submit_answer_arguments_fail_after_one_repair() {
    let invalid_arguments = || {
        json!({
            "claims": [],
            "call_paths": [],
            "diagram": {
                "decision": "not_needed",
                "reason": "A direct answer does not need a diagram."
            }
        })
    };
    let model = Arc::new(MockModelClient::new([
        Ok(submitted_answer_arguments_response(
            "provider-invalid-submit-1",
            invalid_arguments(),
            1,
        )),
        Ok(submitted_answer_arguments_response(
            "provider-invalid-submit-2",
            invalid_arguments(),
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(request("repeated-invalid-submit"), CancellationToken::new())
        .await
        .expect_err("only one invalid submit_answer repair is allowed");

    assert!(matches!(
        error,
        RuntimeError::InvalidFinalAnswer { message }
            if message.contains("missing field `text`")
    ));
}

#[tokio::test]
async fn blank_answer_text_gets_one_validation_repair() {
    let blank = serde_json::to_value(StructuredAnswer {
        text: " \n\t ".to_owned(),
        claims: Vec::new(),
        call_paths: Vec::new(),
        diagram: not_needed_diagram(),
    })
    .expect("structured answer should serialize");
    let model = Arc::new(MockModelClient::new([
        Ok(submitted_answer_arguments_response(
            "provider-blank-answer",
            blank,
            1,
        )),
        Ok(submitted_answer_response(
            "provider-repaired-answer",
            Vec::new(),
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("blank-answer-repair"), CancellationToken::new())
        .await
        .expect("blank answer text should be repairable once");

    assert_eq!(answer.text, "A grounded answer");
    let requests = model.requests();
    let repair_result: Value = serde_json::from_str(
        model_tool_message(&requests[1], "provider-blank-answer")
            .content
            .as_deref()
            .expect("blank answer should receive a tool error"),
    )
    .expect("repair result should be JSON");
    assert_eq!(repair_result["error"]["code"], "invalid_final_answer");
    assert!(
        repair_result["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("non-blank"))
    );
    assert!(requests[1].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("text must contain a non-blank"))
    }));
}

#[tokio::test]
async fn format_and_validation_repairs_have_independent_allowances() {
    let unknown = EvidenceId::from_stable_parts(&["repair-category-unknown"]);
    let model = Arc::new(MockModelClient::new([
        Ok(unstructured_response("answer in the wrong format", 1)),
        Ok(submitted_answer_response(
            "provider-invalid-evidence",
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "An answer with an invalid citation".to_owned(),
                evidence_ids: vec![unknown],
            }],
            1,
        )),
        Ok(submitted_answer_response(
            "provider-valid-after-two-repairs",
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "A repaired answer".to_owned(),
                evidence_ids: Vec::new(),
            }],
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("independent-repairs"), CancellationToken::new())
        .await
        .expect("one format and one validation repair should both be allowed");

    assert_eq!(answer.text, "A grounded answer");
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert!(
        requests[2]
            .tools
            .iter()
            .any(|tool| tool.name == "read_source")
    );
    assert!(requests[2].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("All registered read-only repository tools remain available")
                && content.contains("call them in separate turns")
        })
    }));
}

#[tokio::test]
async fn unknown_call_path_evidence_gets_one_strict_repair_turn() {
    let item = evidence("call-path-repair-known");
    let unknown = EvidenceId::from_stable_parts(&["call-path-repair-unknown"]);
    let target = SymbolId::from_stable_parts(&["call-path-repair-target"]);
    let claim = || StructuredClaim {
        kind: ClaimKind::Fact,
        text: "run is defined in the collected source".to_owned(),
        evidence_ids: vec![item.id],
    };
    let call_path = |evidence_id| StructuredCallPath {
        label: Some("request flow".to_owned()),
        steps: vec![CallPathStep {
            target: TargetResolution::Resolved(target),
            call_edge_id: None,
            evidence_ids: vec![evidence_id],
        }],
        complete: true,
    };
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-read", "read_source", 1)),
        Ok(submitted_answer_with_call_paths(
            "provider-invalid-bindings",
            vec![claim()],
            vec![call_path(unknown)],
            1,
        )),
        Ok(submitted_answer_with_call_paths(
            "provider-repaired-bindings",
            vec![claim()],
            vec![call_path(item.id)],
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"symbol": "run"}),
            evidence: vec![item.clone()],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(request("call-path-repair"), CancellationToken::new(), &sink)
        .await
        .expect("one evidence-binding repair should recover");

    assert_eq!(answer.call_paths.len(), 1);
    assert_eq!(answer.call_paths[0].steps[0].evidence_ids, vec![item.id]);
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert_tool_protocol(
        &requests[2],
        &["provider-read", "provider-invalid-bindings"],
    );
    let repair_result = model_tool_message(&requests[2], "provider-invalid-bindings")
        .content
        .as_deref()
        .expect("invalid bindings should receive a tool result");
    let repair_result: Value =
        serde_json::from_str(repair_result).expect("repair result should be JSON");
    assert_eq!(repair_result["error"]["code"], "invalid_evidence_bindings");
    assert!(
        repair_result["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains(&unknown.to_string()))
    );
    assert!(requests[2].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("Available evidence IDs")
                && content.contains(&item.id.to_string())
                && content.contains("never invent an evidence ID")
        })
    }));
    let mut repair_message = None;
    while let Ok(event) = receiver.try_recv() {
        if let AppEvent::Progress { progress, .. } = event
            && progress.message.contains("final answer validation failed")
        {
            repair_message = Some(progress.message);
        }
    }
    assert!(
        repair_message
            .as_deref()
            .is_some_and(|message| message.contains(&unknown.to_string()))
    );
}

#[tokio::test]
async fn repeatedly_unknown_call_path_evidence_still_fails_strict_validation() {
    let item = evidence("call-path-repeated-known");
    let unknown = EvidenceId::from_stable_parts(&["call-path-repeated-unknown"]);
    let target = SymbolId::from_stable_parts(&["call-path-repeated-target"]);
    let invalid_answer = |provider_call_id| {
        submitted_answer_with_call_paths(
            provider_call_id,
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "run is defined in the collected source".to_owned(),
                evidence_ids: vec![item.id],
            }],
            vec![StructuredCallPath {
                label: Some("request flow".to_owned()),
                steps: vec![CallPathStep {
                    target: TargetResolution::Resolved(target),
                    call_edge_id: None,
                    evidence_ids: vec![unknown],
                }],
                complete: true,
            }],
            1,
        )
    };
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-read", "read_source", 1)),
        Ok(invalid_answer("provider-invalid-bindings-1")),
        Ok(invalid_answer("provider-invalid-bindings-2")),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"symbol": "run"}),
            evidence: vec![item],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(
            request("call-path-repeated-invalid"),
            CancellationToken::new(),
        )
        .await
        .expect_err("only one evidence-binding repair is allowed");

    assert!(matches!(
        error,
        RuntimeError::Evidence(EvidenceValidationError::UnknownCallPathEvidence {
            evidence_id,
            ..
        }) if evidence_id == unknown
    ));
}

#[tokio::test]
async fn needed_diagrams_build_for_all_three_kinds_with_stable_claim_ids() {
    for (index, kind) in [
        DiagramKind::Architecture,
        DiagramKind::Flow,
        DiagramKind::Relationship,
    ]
    .into_iter()
    .enumerate()
    {
        let item = evidence(&format!("needed-diagram-{index}"));
        let model = Arc::new(MockModelClient::new([
            Ok(tool_response("diagram-evidence", "read_source", 1)),
            Ok(final_response_with_diagram(
                vec![StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "The runtime delegates to the service".to_owned(),
                    evidence_ids: vec![item.id],
                }],
                needed_diagram(kind, item.id),
                1,
            )),
        ]));
        let runtime = AgentRuntime::new(
            model,
            Arc::new(ScriptedExecutor::new([ToolStep::Success {
                data: json!({"source": "runtime delegates to service"}),
                evidence: vec![item],
            }])),
            vec![tool("read_source")],
            RuntimeConfig::default(),
        )
        .expect("valid runtime");

        let answer = runtime
            .run_silent(
                request(&format!("needed-diagram-{index}")),
                CancellationToken::new(),
            )
            .await
            .expect("grounded diagram should be accepted");

        let DiagramDecision::Needed { diagram, .. } = answer.diagram else {
            panic!("expected a needed diagram");
        };
        assert_eq!(diagram.kind, kind);
        assert!(diagram.artifact.is_none());
        let claim_id = answer.claims[0].id;
        assert!(
            diagram
                .nodes
                .iter()
                .all(|node| node.claim_ids == vec![claim_id])
        );
        assert!(
            diagram
                .edges
                .iter()
                .all(|edge| edge.claim_ids == vec![claim_id])
        );
    }
}

#[tokio::test]
async fn out_of_range_diagram_claim_indices_get_one_repair_turn() {
    for element in ["node", "edge"] {
        let item = evidence(&format!("out-of-range-{element}"));
        let mut decision = needed_diagram(DiagramKind::Architecture, item.id);
        let StructuredDiagramDecision::Needed { diagram, .. } = &mut decision else {
            panic!("test diagram should be needed");
        };
        if element == "node" {
            diagram.nodes[0].claim_indices = vec![7];
        } else {
            diagram.edges[0].claim_indices = vec![7];
        }
        let model = Arc::new(MockModelClient::new([
            Ok(tool_response("out-of-range-evidence", "read_source", 1)),
            Ok(final_response_with_diagram(
                vec![StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "sensitive source text remains in the grounded answer".to_owned(),
                    evidence_ids: vec![item.id],
                }],
                decision,
                1,
            )),
            Ok(final_response_with_diagram(
                vec![StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "sensitive source text remains in the grounded answer".to_owned(),
                    evidence_ids: vec![item.id],
                }],
                needed_diagram(DiagramKind::Architecture, item.id),
                1,
            )),
        ]));
        let runtime = AgentRuntime::new(
            model.clone(),
            Arc::new(ScriptedExecutor::new([ToolStep::Success {
                data: json!({"source": "sensitive source text"}),
                evidence: vec![item.clone()],
            }])),
            vec![tool("read_source")],
            RuntimeConfig::default(),
        )
        .expect("valid runtime");

        let answer = runtime
            .run_silent(
                request(&format!("out-of-range-{element}")),
                CancellationToken::new(),
            )
            .await
            .expect("the corrected diagram should be accepted");

        assert_eq!(answer.evidence, vec![item]);
        assert!(matches!(answer.diagram, DiagramDecision::Needed { .. }));
        let requests = model.requests();
        assert_eq!(requests.len(), 3);
        assert!(requests[2].messages.iter().any(|message| {
            message.content.as_deref().is_some_and(|content| {
                content.contains("failed strict final-answer validation")
                    && content.contains(&format!(
                        "diagram {element} at index 0 refers to out-of-range claim index 7"
                    ))
            })
        }));
    }
}

#[tokio::test]
async fn diagram_with_a_non_fact_claim_binding_gets_one_repair_turn() {
    let item = evidence("diagram-non-fact");
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("non-fact-evidence", "read_source", 1)),
        Ok(final_response_with_diagram(
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "The runtime may delegate to the service".to_owned(),
                evidence_ids: vec![item.id],
            }],
            needed_diagram(DiagramKind::Architecture, item.id),
            1,
        )),
        Ok(final_response_with_diagram(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "The runtime delegates to the service".to_owned(),
                evidence_ids: vec![item.id],
            }],
            needed_diagram(DiagramKind::Architecture, item.id),
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"source": "runtime delegates to service"}),
            evidence: vec![item.clone()],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("diagram-non-fact"), CancellationToken::new())
        .await
        .expect("the corrected fact binding should be accepted");

    assert_eq!(answer.evidence, vec![item]);
    assert!(matches!(answer.diagram, DiagramDecision::Needed { .. }));
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests[2].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("refers to non-fact claim index 0"))
    }));
}

#[tokio::test]
async fn diagram_evidence_is_derived_from_all_linked_fact_claims() {
    let first = evidence("diagram-first");
    let second = evidence("diagram-second");
    let mut decision = needed_diagram(DiagramKind::Architecture, first.id);
    let StructuredDiagramDecision::Needed { diagram, .. } = &mut decision else {
        panic!("test diagram should be needed");
    };
    for node in &mut diagram.nodes {
        node.claim_indices = vec![0, 1];
    }
    diagram.edges[0].claim_indices = vec![0, 1];
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("diagram-evidence", "read_source", 1)),
        Ok(final_response_with_diagram(
            vec![
                StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "The runtime delegates to the service".to_owned(),
                    evidence_ids: vec![first.id],
                },
                StructuredClaim {
                    kind: ClaimKind::Fact,
                    text: "The service returns a result".to_owned(),
                    evidence_ids: vec![second.id],
                },
            ],
            decision,
            1,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"source": "two independent excerpts"}),
            evidence: vec![first.clone(), second.clone()],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(
            request("diagram-derived-evidence"),
            CancellationToken::new(),
        )
        .await
        .expect("runtime-derived element evidence should validate");

    assert_eq!(answer.evidence, vec![first.clone(), second.clone()]);
    let DiagramDecision::Needed { diagram, .. } = answer.diagram else {
        panic!("expected a needed diagram");
    };
    let expected = vec![first.id, second.id];
    assert!(
        diagram
            .nodes
            .iter()
            .all(|node| node.evidence_ids == expected)
    );
    assert_eq!(diagram.edges[0].evidence_ids, expected);
}

#[tokio::test]
async fn diagram_with_a_dangling_edge_gets_one_repair_turn() {
    let item = evidence("diagram-dangling-edge");
    let mut decision = needed_diagram(DiagramKind::Flow, item.id);
    let StructuredDiagramDecision::Needed { diagram, .. } = &mut decision else {
        panic!("test diagram should be needed");
    };
    diagram.edges[0].target = "missing-node".to_owned();
    let claims = vec![StructuredClaim {
        kind: ClaimKind::Fact,
        text: "The runtime delegates to the service".to_owned(),
        evidence_ids: vec![item.id],
    }];
    let submitted = |provider_call_id: &str, diagram| {
        submitted_answer_arguments_response(
            provider_call_id,
            serde_json::to_value(StructuredAnswer {
                text: "A grounded answer".to_owned(),
                claims: claims.clone(),
                call_paths: Vec::new(),
                diagram,
            })
            .expect("structured answer should serialize"),
            1,
        )
    };
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("dangling-evidence", "read_source", 1)),
        Ok(submitted("provider-invalid-diagram", decision)),
        Ok(submitted(
            "provider-repaired-diagram",
            needed_diagram(DiagramKind::Flow, item.id),
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"source": "runtime delegates to service"}),
            evidence: vec![item],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("diagram-dangling-edge"), CancellationToken::new())
        .await
        .expect("the corrected topology should be accepted");

    assert!(matches!(answer.diagram, DiagramDecision::Needed { .. }));
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert_tool_protocol(
        &requests[2],
        &["dangling-evidence", "provider-invalid-diagram"],
    );
    let repair_result = model_tool_message(&requests[2], "provider-invalid-diagram")
        .content
        .as_deref()
        .expect("invalid diagram should receive a tool result");
    let repair_result: Value =
        serde_json::from_str(repair_result).expect("repair result should be JSON");
    assert_eq!(repair_result["error"]["code"], "invalid_diagram");
    assert!(requests[2].messages.iter().any(|message| {
        message
            .content
            .as_deref()
            .is_some_and(|content| content.contains("unknown target node"))
    }));
}

#[tokio::test]
async fn repeatedly_invalid_diagram_is_rejected_instead_of_marked_not_needed() {
    let item = evidence("diagram-repeated-dangling-edge");
    let mut decision = needed_diagram(DiagramKind::Architecture, item.id);
    let StructuredDiagramDecision::Needed { diagram, .. } = &mut decision else {
        panic!("test diagram should be needed");
    };
    diagram.edges[0].target = "missing-node".to_owned();
    let invalid_response = || {
        final_response_with_diagram(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "The runtime delegates to the service".to_owned(),
                evidence_ids: vec![item.id],
            }],
            decision.clone(),
            1,
        )
    };
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("repeated-diagram-evidence", "read_source", 1)),
        Ok(invalid_response()),
        Ok(invalid_response()),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"source": "runtime delegates to service"}),
            evidence: vec![item],
        }])),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(
            request("draw the architecture diagram"),
            CancellationToken::new(),
        )
        .await
        .expect_err("a repeatedly invalid requested diagram must fail honestly");

    assert!(matches!(
        error,
        RuntimeError::Evidence(EvidenceValidationError::UnknownDiagramEdgeTarget { edge_index: 0 })
    ));
    assert_eq!(model.requests().len(), 3);
}

#[tokio::test]
async fn missing_diagram_unstructured_text_gets_one_repair_turn() {
    let model = Arc::new(MockModelClient::new([
        Ok(unstructured_response(
            r#"{"text":"answer","claims":[],"call_paths":[]}"#,
            2,
        )),
        Ok(submitted_answer_response(
            "provider-repaired-submit",
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "A grounded answer".to_owned(),
                evidence_ids: Vec::new(),
            }],
            3,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(request("format-repair"), CancellationToken::new(), &sink)
        .await
        .expect("one format repair should recover");

    assert_eq!(answer.text, "A grounded answer");
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].messages.iter().any(|message| {
        message.content.as_deref().is_some_and(|content| {
            content.contains("Call submit_answer exactly once")
                && content.contains("an explicit diagram decision")
                && content.contains("claim_indices")
        })
    }));
    let mut saw_repair = false;
    while let Ok(event) = receiver.try_recv() {
        if matches!(
            event,
            AppEvent::Progress { progress, .. }
                if progress.message.contains("structured answer repair")
        ) {
            saw_repair = true;
        }
    }
    assert!(saw_repair);
}

#[tokio::test]
async fn repeated_unstructured_text_fails_with_a_bounded_preview() {
    let model = Arc::new(MockModelClient::new([
        Ok(unstructured_response("first prose response", 1)),
        Ok(unstructured_response(&"x".repeat(500), 1)),
    ]));
    let runtime = AgentRuntime::new(
        model,
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(request("repeated-unstructured"), CancellationToken::new())
        .await
        .expect_err("only one repair turn is allowed");

    let RuntimeError::UnstructuredFinalAnswer { preview } = error else {
        panic!("expected unstructured final-answer error");
    };
    assert!(preview.len() < 500);
    assert!(preview.ends_with("..."));
}

#[tokio::test]
async fn retryable_model_failures_do_not_truncate_tool_source() {
    let retry_context = "fresh retry context\n".repeat(300);
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("retry-read", "read_source", 1)),
        Err(ModelError::Http {
            status: 504,
            message: "gateway timeout".to_owned(),
            retryable: true,
        }),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "The retry recovered".to_owned(),
                evidence_ids: Vec::new(),
            }],
            3,
        )),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::new([ToolStep::Success {
            data: json!({"content": retry_context.clone()}),
            evidence: Vec::new(),
        }])),
        vec![tool("read_source")],
        RuntimeConfig {
            max_model_retries: 2,
            max_gateway_retries: 2,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    let answer = runtime
        .run(request("retryable-model"), CancellationToken::new(), &sink)
        .await
        .expect("retryable gateway error should recover");

    assert_eq!(answer.text, "A grounded answer");
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert_tool_protocol(&requests[1], &["retry-read"]);
    assert_tool_protocol(&requests[2], &["retry-read"]);
    let initial_tool_content = model_tool_message(&requests[1], "retry-read")
        .content
        .as_deref()
        .expect("initial attempt should contain tool output");
    let retry_tool_content = model_tool_message(&requests[2], "retry-read")
        .content
        .as_deref()
        .expect("retry should contain compacted tool output");
    assert_eq!(initial_tool_content, retry_tool_content);
    let retry_tool_content: Value =
        serde_json::from_str(retry_tool_content).expect("retry tool output should remain JSON");
    assert_eq!(retry_tool_content["data"]["content"], json!(retry_context));
    let mut saw_retry = false;
    while let Ok(event) = receiver.try_recv() {
        if matches!(
            event,
            AppEvent::Progress { progress, .. }
                if progress.message.contains("HTTP 504")
                    && progress.message.contains("retrying 1/2")
                    && !progress.message.contains("with compacted tool context")
        ) {
            saw_retry = true;
        }
    }
    assert!(saw_retry);
}

#[tokio::test]
async fn exhausted_retryable_model_failures_report_attempt_count() {
    let gateway_timeout = || {
        Err(ModelError::Http {
            status: 504,
            message: "gateway timeout".to_owned(),
            retryable: true,
        })
    };
    let model = Arc::new(MockModelClient::new([
        gateway_timeout(),
        gateway_timeout(),
        gateway_timeout(),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            max_model_retries: 2,
            max_gateway_retries: 2,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(request("gateway-timeout"), CancellationToken::new())
        .await
        .expect_err("all gateway attempts should fail");
    let app_error = error.to_app_error();

    assert!(matches!(
        error,
        RuntimeError::ModelRetriesExhausted {
            attempts: 3,
            source: ModelError::Http { status: 504, .. }
        }
    ));
    assert_eq!(model.requests().len(), 3);
    assert_eq!(app_error.code, "model_error");
    assert!(app_error.retryable);
    assert!(app_error.message.contains("failed after 3 attempts"));
}

#[tokio::test]
async fn default_gateway_policy_survives_five_fast_failures() {
    let gateway_error = || {
        Err(ModelError::Http {
            status: 502,
            message: "Bad Gateway".to_owned(),
            retryable: true,
        })
    };
    let model = Arc::new(MockModelClient::new([
        gateway_error(),
        gateway_error(),
        gateway_error(),
        gateway_error(),
        gateway_error(),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    runtime
        .run_silent(request("fast-gateway-recovery"), CancellationToken::new())
        .await
        .expect("the default gateway policy should allow five recovery retries");

    assert_eq!(model.requests().len(), 6);
}

#[tokio::test]
async fn non_retryable_model_failures_are_not_retried() {
    let model = Arc::new(MockModelClient::new([
        Err(ModelError::InvalidRequest {
            message: "bad request".to_owned(),
        }),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            max_model_retries: 2,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    let error = runtime
        .run_silent(request("non-retryable-model"), CancellationToken::new())
        .await
        .expect_err("invalid requests must fail immediately");

    assert!(matches!(
        error,
        RuntimeError::Model(ModelError::InvalidRequest { .. })
    ));
    assert_eq!(model.requests().len(), 1);
    assert_eq!(model.remaining(), 1);
}

#[tokio::test]
async fn transient_invalid_model_response_is_retried() {
    let model = Arc::new(MockModelClient::new([
        Err(ModelError::InvalidResponse {
            message: "truncated JSON response".to_owned(),
        }),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig {
            max_model_retries: 1,
            model_retry_initial_delay: Duration::ZERO,
            ..RuntimeConfig::default()
        },
    )
    .expect("valid runtime");

    runtime
        .run_silent(request("invalid-response-retry"), CancellationToken::new())
        .await
        .expect("a transient malformed provider response should recover");

    assert_eq!(model.requests().len(), 2);
}

#[tokio::test]
async fn unknown_tool_is_returned_to_the_model_without_executor_dispatch() {
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("provider-1", "run_shell", 1)),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let executor = Arc::new(ScriptedExecutor::default());
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("unknown-tool"), CancellationToken::new())
        .await
        .expect("the model should recover after receiving an unknown-tool result");

    assert_eq!(answer.text, "A grounded answer");
    assert!(executor.calls().is_empty());
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let result = model_tool_message(&requests[1], "provider-1")
        .content
        .as_deref()
        .expect("unknown tool result should have content");
    assert!(result.contains("unknown_tool"));
}

#[tokio::test]
async fn exploration_continues_until_an_answer_without_hiding_tools() {
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("exploration-call-1", "read_source", 1)),
        Ok(tool_response("exploration-call-2", "read_source", 1)),
        Ok(submitted_answer_response(
            "submit-call",
            vec![StructuredClaim {
                kind: ClaimKind::Inference,
                text: "The collected source is sufficient for an answer".to_owned(),
                evidence_ids: Vec::new(),
            }],
            1,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([
        ToolStep::Success {
            data: json!({"source": "pub fn run() {}"}),
            evidence: Vec::new(),
        },
        ToolStep::Success {
            data: json!({"source": "pub fn stop() {}"}),
            evidence: Vec::new(),
        },
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor.clone(),
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("unbounded-exploration"), CancellationToken::new())
        .await
        .expect("compatibility limits must not stop exploration");

    assert_eq!(answer.text, "A grounded answer");
    assert_eq!(executor.calls().len(), 2);
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request.tools.iter().any(|tool| tool.name == "read_source")
            && request
                .tools
                .iter()
                .any(|tool| tool.name == SUBMIT_ANSWER_TOOL_NAME)
    }));
    assert!(
        requests
            .iter()
            .flat_map(|request| &request.messages)
            .all(|message| {
                !message
                    .content
                    .as_deref()
                    .is_some_and(|content| content.contains("step budget is exhausted"))
            })
    );
}

#[tokio::test]
async fn tool_errors_are_returned_to_the_model_as_error_outputs() {
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("failed-call", "read_source", 2)),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Unknown,
                text: "The source could not be read".to_owned(),
                evidence_ids: Vec::new(),
            }],
            2,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([ToolStep::Failure(
        ToolError::Execution {
            name: "read_source".to_owned(),
            message: "backend unavailable".to_owned(),
        },
    )]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor,
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    let (sink, mut receiver) = ChannelEventSink::channel();

    runtime
        .run(request("tool-error"), CancellationToken::new(), &sink)
        .await
        .expect("model should recover from tool error");

    let requests = model.requests();
    let tool_message = requests[1]
        .messages
        .iter()
        .find(|message| message.tool_call_id.as_deref() == Some("failed-call"))
        .expect("tool error message should be returned to model");
    assert!(
        tool_message
            .content
            .as_deref()
            .expect("tool content")
            .contains("backend unavailable")
    );
    let model_error: Value =
        serde_json::from_str(tool_message.content.as_deref().expect("tool error content"))
            .expect("tool error should remain JSON");
    assert!(model_error["error"].get("details").is_some());
    let mut saw_error_output = false;
    while let Ok(event) = receiver.try_recv() {
        if let AppEvent::ToolCallCompleted { output, .. } = event
            && output.is_error
        {
            saw_error_output = true;
            assert!(output.result["error"].get("details").is_some());
        }
    }
    assert!(saw_error_output);
}

#[tokio::test]
async fn duplicate_evidence_excerpt_variants_merge_deterministically() {
    let mut original = evidence("excerpt-variant");
    original.excerpt = Some("short source".to_owned());
    let mut richer = original.clone();
    richer.excerpt = Some("short source with additional surrounding context".to_owned());
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("excerpt-1", "read_source", 1)),
        Ok(tool_response("excerpt-2", "read_source", 1)),
        Ok(final_response(
            vec![StructuredClaim {
                kind: ClaimKind::Fact,
                text: "The source supports the answer".to_owned(),
                evidence_ids: vec![original.id],
            }],
            1,
        )),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([
        ToolStep::Success {
            data: json!({"source": original.excerpt.clone()}),
            evidence: vec![original.clone()],
        },
        ToolStep::Success {
            data: json!({"match": "same evidence location"}),
            evidence: vec![richer.clone()],
        },
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor,
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(request("excerpt-variant"), CancellationToken::new())
        .await
        .expect("excerpt variants must not fail an otherwise consistent answer");

    assert_eq!(answer.evidence, vec![richer]);
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let updated: Value = serde_json::from_str(
        model_tool_message(&requests[2], "excerpt-2")
            .content
            .as_deref()
            .expect("updated evidence should be visible to the model"),
    )
    .expect("updated evidence should remain JSON");
    assert_eq!(
        updated["evidence"][0]["excerpt"],
        json!("short source with additional surrounding context")
    );
}

#[tokio::test]
async fn conflicting_evidence_identity_is_returned_to_model_for_recovery() {
    let original = evidence("conflicting-evidence-identity");
    let mut conflicting = original.clone();
    conflicting.path = RepositoryPath::new("src/different.rs").expect("valid path");
    let model = Arc::new(MockModelClient::new([
        Ok(tool_response("conflict-1", "read_source", 1)),
        Ok(tool_response("conflict-2", "read_source", 1)),
        Ok(final_response(Vec::new(), 1)),
    ]));
    let executor = Arc::new(ScriptedExecutor::new([
        ToolStep::Success {
            data: json!({"source": original.excerpt.clone()}),
            evidence: vec![original.clone()],
        },
        ToolStep::Success {
            data: json!({"source": conflicting.excerpt.clone()}),
            evidence: vec![conflicting],
        },
    ]));
    let runtime = AgentRuntime::new(
        model.clone(),
        executor,
        vec![tool("read_source")],
        RuntimeConfig::default(),
    )
    .expect("valid runtime");

    let answer = runtime
        .run_silent(
            request("conflicting-evidence-identity"),
            CancellationToken::new(),
        )
        .await
        .expect("a conflicting tool result should not discard the whole answer");

    assert_eq!(answer.text, "A grounded answer");
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    let conflict = model_tool_message(&requests[2], "conflict-2")
        .content
        .as_deref()
        .expect("conflict should be returned as a tool error");
    assert!(conflict.contains("conflicting_evidence"));
}

#[tokio::test]
async fn facts_without_evidence_and_unknown_references_are_rejected() {
    let unsupported = final_response(
        vec![StructuredClaim {
            kind: ClaimKind::Fact,
            text: "Unsupported fact".to_owned(),
            evidence_ids: Vec::new(),
        }],
        1,
    );
    let unsupported_runtime = AgentRuntime::new(
        Arc::new(MockModelClient::new([
            Ok(unsupported.clone()),
            Ok(unsupported),
        ])),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    assert!(matches!(
        unsupported_runtime
            .run_silent(request("unsupported"), CancellationToken::new())
            .await,
        Err(RuntimeError::Evidence(
            EvidenceValidationError::FactWithoutEvidence { .. }
        ))
    ));

    let unknown_id = EvidenceId::from_stable_parts(&["never-returned"]);
    let unknown = final_response(
        vec![StructuredClaim {
            kind: ClaimKind::Inference,
            text: "Inference with a bad citation".to_owned(),
            evidence_ids: vec![unknown_id],
        }],
        1,
    );
    let unknown_runtime = AgentRuntime::new(
        Arc::new(MockModelClient::new([Ok(unknown.clone()), Ok(unknown)])),
        Arc::new(ScriptedExecutor::default()),
        Vec::new(),
        RuntimeConfig::default(),
    )
    .expect("valid runtime");
    assert!(matches!(
        unknown_runtime
            .run_silent(request("unknown-evidence"), CancellationToken::new())
            .await,
        Err(RuntimeError::Evidence(
            EvidenceValidationError::UnknownClaimEvidence {
                evidence_id,
                ..
            }
        )) if evidence_id == unknown_id
    ));
}

struct PendingModel {
    entered: Mutex<Option<oneshot::Sender<()>>>,
}

#[async_trait]
impl ModelClient for PendingModel {
    async fn complete(&self, _request: ModelRequest) -> Result<ModelResponse, ModelError> {
        let sender = self
            .entered
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(sender) = sender {
            let _ = sender.send(());
        }
        pending().await
    }
}

#[tokio::test]
async fn cancellation_interrupts_an_in_flight_model_without_sleeping() {
    let (entered_sender, entered_receiver) = oneshot::channel();
    let runtime = Arc::new(
        AgentRuntime::new(
            Arc::new(PendingModel {
                entered: Mutex::new(Some(entered_sender)),
            }),
            Arc::new(ScriptedExecutor::default()),
            Vec::new(),
            RuntimeConfig {
                timeout: Duration::from_secs(30),
                ..RuntimeConfig::default()
            },
        )
        .expect("valid runtime"),
    );
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let task = tokio::spawn(async move { runtime.run_silent(request("cancel"), task_token).await });

    entered_receiver
        .await
        .expect("model should signal that it is pending");
    cancellation.cancel();
    let result = task.await.expect("runtime task should join");

    assert!(matches!(result, Err(RuntimeError::Cancelled)));
}

#[tokio::test(start_paused = true)]
async fn long_model_wait_emits_elapsed_time_heartbeat() {
    let (entered_sender, entered_receiver) = oneshot::channel();
    let runtime = Arc::new(
        AgentRuntime::new(
            Arc::new(PendingModel {
                entered: Mutex::new(Some(entered_sender)),
            }),
            Arc::new(ScriptedExecutor::default()),
            Vec::new(),
            RuntimeConfig {
                timeout: Duration::from_secs(30),
                ..RuntimeConfig::default()
            },
        )
        .expect("valid runtime"),
    );
    let cancellation = CancellationToken::new();
    let task_token = cancellation.clone();
    let (sink, mut receiver) = ChannelEventSink::channel();
    let task =
        tokio::spawn(async move { runtime.run(request("heartbeat"), task_token, &sink).await });

    entered_receiver
        .await
        .expect("model should signal that it is pending");
    tokio::time::advance(Duration::from_secs(6)).await;
    tokio::task::yield_now().await;

    let mut events = Vec::new();
    while let Ok(event) = receiver.try_recv() {
        events.push(event);
    }
    assert!(events.iter().any(|event| matches!(
        event,
        AppEvent::Progress { progress, .. }
            if progress.message.contains("waiting for model response")
                && progress.message.contains("s elapsed")
    )));

    cancellation.cancel();
    assert!(matches!(
        task.await.expect("runtime task should join"),
        Err(RuntimeError::Cancelled)
    ));
}

#[tokio::test(start_paused = true)]
async fn runtime_timeout_interrupts_an_in_flight_model_without_wall_clock_sleep() {
    let (entered_sender, entered_receiver) = oneshot::channel();
    let runtime = Arc::new(
        AgentRuntime::new(
            Arc::new(PendingModel {
                entered: Mutex::new(Some(entered_sender)),
            }),
            Arc::new(ScriptedExecutor::default()),
            Vec::new(),
            RuntimeConfig {
                timeout: Duration::from_secs(5),
                ..RuntimeConfig::default()
            },
        )
        .expect("valid runtime"),
    );
    let task = tokio::spawn(async move {
        runtime
            .run_silent(request("timeout"), CancellationToken::new())
            .await
    });

    entered_receiver
        .await
        .expect("model should signal that it is pending");
    tokio::time::advance(Duration::from_secs(6)).await;
    let result = task.await.expect("runtime task should join");

    let error = result.expect_err("the overall deadline should interrupt the model");
    assert!(matches!(
        &error,
        RuntimeError::Timeout { limit, operation }
            if *limit == Duration::from_secs(5)
                && *operation == "waiting for a model response"
    ));
    let app_error = error.to_app_error();
    assert_eq!(app_error.code, "runtime_timeout");
    assert!(app_error.retryable);
    assert!(app_error.message.contains("overall timeout of 5s"));
    assert!(app_error.message.contains("waiting for a model response"));
    assert!(
        app_error
            .message
            .contains("includes exploration, model retries, and answer repair")
    );
}
