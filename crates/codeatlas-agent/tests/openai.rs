#![allow(clippy::expect_used, clippy::too_many_lines)]

use std::time::Duration;

use codeatlas_agent::{
    AssistantOutput, ModelClient, ModelError, ModelMessage, ModelRequest, OpenAiChatClient,
    OpenAiClientBuildError, RuntimeSecretHeaders, SecretString, StructuredDiagramDecision,
};
use codeatlas_core::{ClaimKind, EvidenceId, ModelConfig, TokenUsage, ToolDefinition};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::oneshot,
    task::JoinHandle,
};

async fn serve_once(
    status: u16,
    reason: &str,
    body: String,
) -> (String, oneshot::Receiver<String>, JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local mock listener should bind");
    let address = listener.local_addr().expect("listener address");
    let endpoint = format!("http://{address}/v1/chat/completions");
    let reason = reason.to_owned();
    let (request_sender, request_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("mock request connection");
        let request = read_request(&mut stream).await;
        let _ = request_sender.send(request);
        let response = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .await
            .expect("mock response should write");
        stream.shutdown().await.expect("mock stream shutdown");
    });
    (endpoint, request_receiver, task)
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> String {
    let mut request = Vec::new();
    let mut content_length = None;
    let mut header_end = None;
    loop {
        let mut chunk = [0_u8; 4096];
        let read = stream
            .read(&mut chunk)
            .await
            .expect("mock request should read");
        assert!(read > 0, "connection closed before complete request");
        request.extend_from_slice(&chunk[..read]);
        if header_end.is_none() {
            header_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|position| position + 4);
            if let Some(end) = header_end {
                let headers = std::str::from_utf8(&request[..end]).expect("UTF-8 headers");
                content_length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().expect("content length"))
                });
            }
        }
        if let (Some(end), Some(length)) = (header_end, content_length) {
            if request.len() >= end + length {
                break;
            }
        }
    }
    String::from_utf8(request).expect("HTTP request should be UTF-8")
}

async fn hold_one_request() -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local mock listener should bind");
    let address = listener.local_addr().expect("listener address");
    let endpoint = format!("http://{address}/v1/chat/completions");
    let (accepted_sender, accepted_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("mock request connection");
        let _request = read_request(&mut stream).await;
        let _ = accepted_sender.send(());
        let _ = release_receiver.await;
    });
    (endpoint, accepted_receiver, release_sender, task)
}

async fn hold_response_body() -> (
    String,
    oneshot::Receiver<()>,
    oneshot::Sender<()>,
    JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("local mock listener should bind");
    let address = listener.local_addr().expect("listener address");
    let endpoint = format!("http://{address}/v1/chat/completions");
    let (headers_sender, headers_receiver) = oneshot::channel();
    let (release_sender, release_receiver) = oneshot::channel();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("mock request connection");
        let _request = read_request(&mut stream).await;
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 100\r\nconnection: close\r\n\r\n{",
            )
            .await
            .expect("mock response headers should write");
        let _ = headers_sender.send(());
        let _ = release_receiver.await;
    });
    (endpoint, headers_receiver, release_sender, task)
}

fn model_config(endpoint: String) -> ModelConfig {
    ModelConfig {
        endpoint,
        model: "test-model".to_owned(),
        reasoning_mode: None,
        reasoning_effort: None,
        temperature: Some(0.25),
        max_output_tokens: Some(321),
        context_window_tokens: Some(8_192),
    }
}

fn tool_definition() -> ToolDefinition {
    ToolDefinition {
        name: "read_source".to_owned(),
        description: "Read indexed source".to_owned(),
        input_schema: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
        output_schema: Some(json!({"type": "object"})),
    }
}

fn request() -> ModelRequest {
    ModelRequest {
        messages: vec![
            ModelMessage::system("read-only system"),
            ModelMessage::user("Where is run?"),
        ],
        tools: vec![tool_definition()],
    }
}

#[tokio::test]
async fn openai_client_serializes_config_tools_and_parses_tool_calls_and_usage() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": "call-provider-1",
                    "type": "function",
                    "function": {
                        "name": "read_source",
                        "arguments": "{\"path\":\"src/lib.rs\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {
            "prompt_tokens": 20,
            "completion_tokens": 5,
            "total_tokens": 25,
            "prompt_tokens_details": {"cached_tokens": 7}
        }
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let client = OpenAiChatClient::new(
        model_config(endpoint),
        RuntimeSecretHeaders::bearer(SecretString::new("local-test-key"))
            .expect("valid secret header"),
    )
    .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("tool-call response should parse");
    let AssistantOutput::ToolCalls { calls, .. } = result.output else {
        panic!("expected tool calls");
    };
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].id, "call-provider-1");
    assert_eq!(calls[0].name, "read_source");
    assert_eq!(calls[0].arguments, json!({"path": "src/lib.rs"}));
    assert_eq!(
        result.usage,
        Some(TokenUsage {
            input_tokens: 20,
            output_tokens: 5,
            cached_input_tokens: 7,
            total_tokens: 25,
        })
    );

    let raw_request = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
    assert!(
        raw_request
            .to_ascii_lowercase()
            .contains("authorization: bearer local-test-key")
    );
    let (_, body) = raw_request
        .split_once("\r\n\r\n")
        .expect("HTTP request body");
    let payload: Value = serde_json::from_str(body).expect("request JSON");
    assert_eq!(payload["model"], "test-model");
    assert_eq!(payload["temperature"], 0.25);
    assert_eq!(payload["max_tokens"], 321);
    assert_eq!(payload["stream"], false);
    assert_eq!(payload["tool_choice"], "auto");
    assert!(payload.get("reasoning_mode").is_none());
    assert!(payload.get("reasoning_effort").is_none());
    assert_eq!(payload["tools"][0]["type"], "function");
    assert_eq!(payload["tools"][0]["function"]["name"], "read_source");
    assert_eq!(
        payload["tools"][0]["function"]["parameters"]["required"][0],
        "path"
    );
}

#[tokio::test]
async fn openai_client_transmits_optional_reasoning_configuration() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "{\"text\":\"answer\",\"claims\":[],\"call_paths\":[],\"diagram\":{\"decision\":\"not_needed\",\"reason\":\"not useful\"}}"
            },
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let mut config = model_config(endpoint);
    config.reasoning_mode = Some("enabled".to_owned());
    config.reasoning_effort = Some("high".to_owned());
    let client = OpenAiChatClient::new(config, RuntimeSecretHeaders::new()).expect("valid client");

    client
        .complete(request())
        .await
        .expect("reasoning request should complete");
    let raw_request = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
    let (_, body) = raw_request
        .split_once("\r\n\r\n")
        .expect("HTTP request body");
    let payload: Value = serde_json::from_str(body).expect("request JSON");
    assert_eq!(payload["reasoning_mode"], "enabled");
    assert_eq!(payload["reasoning_effort"], "high");
}

#[tokio::test]
async fn openai_client_parses_final_structured_answer() {
    let evidence_id = EvidenceId::from_stable_parts(&["openai-final-evidence"]);
    let answer_json = json!({
        "text": "run is in src/lib.rs",
        "claims": [{
            "kind": "fact",
            "text": "run is in src/lib.rs",
            "evidence_ids": [evidence_id]
        }],
        "call_paths": [],
        "diagram": {
            "decision": "not_needed",
            "reason": "A direct fact does not need a diagram."
        }
    })
    .to_string();
    let response = json!({
        "choices": [{
            "message": {"role": "assistant", "content": answer_json},
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 9,
            "total_tokens": 17
        }
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("final answer should parse");

    let AssistantOutput::FinalAnswer { answer } = result.output else {
        panic!("expected final answer");
    };
    assert_eq!(answer.text, "run is in src/lib.rs");
    assert_eq!(answer.claims[0].kind, ClaimKind::Fact);
    assert_eq!(answer.claims[0].evidence_ids, vec![evidence_id]);
    assert!(matches!(
        answer.diagram,
        StructuredDiagramDecision::NotNeeded { .. }
    ));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn openai_client_treats_json_without_diagram_as_unstructured_for_repair() {
    let answer_json = json!({
        "text": "legacy-shaped model answer",
        "claims": [],
        "call_paths": []
    })
    .to_string();
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": answer_json.clone()
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 9,
            "total_tokens": 17
        }
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("missing diagram is a repairable format response");

    assert!(matches!(
        result.output,
        AssistantOutput::UnstructuredText { content } if content == answer_json
    ));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn openai_client_extracts_structured_answer_from_markdown_or_prose() {
    let answer_json = json!({
        "text": "compact answer",
        "claims": [{
            "kind": "inference",
            "text": "compact answer",
            "evidence_ids": []
        }],
        "call_paths": [],
        "diagram": {
            "decision": "not_needed",
            "reason": "A short explanation does not need a diagram."
        }
    })
    .to_string();
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": format!("Here is the result:\n```json\n{answer_json}\n```")
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 9,
            "total_tokens": 17
        }
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("embedded structured answer should parse");

    let AssistantOutput::FinalAnswer { answer } = result.output else {
        panic!("expected final answer");
    };
    assert_eq!(answer.text, "compact answer");
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn openai_client_preserves_plain_text_for_runtime_repair() {
    let response = json!({
        "choices": [{
            "message": {
                "role": "assistant",
                "content": "This is a plain answer rather than JSON."
            },
            "finish_reason": "stop"
        }],
        "usage": {
            "prompt_tokens": 8,
            "completion_tokens": 9,
            "total_tokens": 17
        }
    })
    .to_string();
    let (endpoint, raw_request, server) = serve_once(200, "OK", response).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("plain assistant content is a recoverable model output");

    assert!(matches!(
        result.output,
        AssistantOutput::UnstructuredText { content }
            if content == "This is a plain answer rather than JSON."
    ));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn missing_or_null_usage_remains_unknown() {
    for usage in [None, Some(Value::Null)] {
        let mut response = json!({
            "choices": [{
                "message": {"role": "assistant", "content": "plain answer"},
                "finish_reason": "stop"
            }]
        });
        if let Some(usage) = usage {
            response["usage"] = usage;
        }
        let (endpoint, raw_request, server) = serve_once(200, "OK", response.to_string()).await;
        let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
            .expect("valid client");

        let result = client
            .complete(request())
            .await
            .expect("valid response without usage should be accepted");

        assert_eq!(result.usage, None);
        let _ = raw_request.await.expect("captured HTTP request");
        server.await.expect("mock server should finish");
    }
}

#[tokio::test]
async fn context_overflow_client_errors_are_retryable_but_other_bad_requests_are_not() {
    let cases = [
        (
            400,
            json!({"error": {
                "code": "context_length_exceeded",
                "message": "This model's maximum context length is 8192 tokens."
            }}),
            true,
        ),
        (
            413,
            json!({"error": {"message": "Prompt is too long: 9000 tokens"}}),
            true,
        ),
        (
            400,
            json!({"error": {"message": "Invalid tool schema"}}),
            false,
        ),
        (
            413,
            json!({"error": {"message": "Uploaded payload is too large"}}),
            false,
        ),
    ];

    for (status, response, expected_retryable) in cases {
        let (endpoint, raw_request, server) =
            serve_once(status, "Client Error", response.to_string()).await;
        let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
            .expect("valid client");

        let error = client
            .complete(request())
            .await
            .expect_err("HTTP client error should fail");

        assert!(matches!(
            error,
            ModelError::Http { retryable, .. } if retryable == expected_retryable
        ));
        let _ = raw_request.await.expect("captured HTTP request");
        server.await.expect("mock server should finish");
    }
}

#[tokio::test]
async fn length_finish_reason_reports_truncated_content_and_tool_arguments() {
    let responses = [
        json!({
            "choices": [{
                "message": {"role": "assistant", "content": "{\"text\":\"partial"},
                "finish_reason": "length"
            }]
        }),
        json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call-truncated",
                        "type": "function",
                        "function": {"name": "read_source", "arguments": "{\"path\":\"src"}
                    }]
                },
                "finish_reason": "length"
            }]
        }),
    ];

    for response in responses {
        let (endpoint, raw_request, server) = serve_once(200, "OK", response.to_string()).await;
        let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
            .expect("valid client");

        let error = client
            .complete(request())
            .await
            .expect_err("length-limited incomplete response should fail");

        assert!(matches!(error, ModelError::InvalidResponse { .. }));
        assert!(error.to_string().contains("finish_reason=length"));
        assert!(error.to_string().contains("truncated"));
        let _ = raw_request.await.expect("captured HTTP request");
        server.await.expect("mock server should finish");
    }
}

#[tokio::test]
async fn length_finish_reason_accepts_a_complete_structured_answer() {
    let response = json!({
        "choices": [{
            "message": {"role": "assistant", "content": json!({
                "text": "complete answer",
                "claims": [],
                "call_paths": [],
                "diagram": {
                    "decision": "not_needed",
                    "reason": "No diagram is needed."
                }
            }).to_string()},
            "finish_reason": "length"
        }]
    });
    let (endpoint, raw_request, server) = serve_once(200, "OK", response.to_string()).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let result = client
        .complete(request())
        .await
        .expect("complete structured content should remain usable");

    assert!(matches!(
        result.output,
        AssistantOutput::FinalAnswer { ref answer } if answer.text == "complete answer"
    ));
    assert_eq!(result.finish_reason.as_deref(), Some("length"));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[test]
fn openai_client_rejects_invalid_construction_boundaries() {
    for temperature in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
        let mut config = model_config("https://example.com/v1/chat/completions".to_owned());
        config.temperature = Some(temperature);
        assert!(matches!(
            OpenAiChatClient::new(config, RuntimeSecretHeaders::new()),
            Err(OpenAiClientBuildError::NonFiniteTemperature)
        ));
    }

    let mut config = model_config("https://example.com/v1/chat/completions".to_owned());
    config.model = " \t".to_owned();
    assert!(matches!(
        OpenAiChatClient::new(config, RuntimeSecretHeaders::new()),
        Err(OpenAiClientBuildError::EmptyModel)
    ));

    let mut config = model_config("https://example.com/v1/chat/completions".to_owned());
    config.max_output_tokens = Some(0);
    assert!(matches!(
        OpenAiChatClient::new(config, RuntimeSecretHeaders::new()),
        Err(OpenAiClientBuildError::ZeroMaxOutputTokens)
    ));

    let config = model_config("file:///tmp/chat-completions".to_owned());
    assert!(matches!(
        OpenAiChatClient::new(config, RuntimeSecretHeaders::new()),
        Err(OpenAiClientBuildError::InvalidEndpoint { .. })
    ));

    let config = model_config("http://models.example.com/v1/chat/completions".to_owned());
    let error = OpenAiChatClient::new(config, RuntimeSecretHeaders::new())
        .expect_err("remote plaintext HTTP endpoint must be rejected");
    assert!(error.to_string().contains("must use HTTPS"));

    for endpoint in [
        "http://localhost:11434/v1/chat/completions",
        "http://127.0.0.1:11434/v1/chat/completions",
        "http://[::1]:11434/v1/chat/completions",
    ] {
        OpenAiChatClient::new(
            model_config(endpoint.to_owned()),
            RuntimeSecretHeaders::new(),
        )
        .expect("explicit loopback HTTP endpoint should remain supported");
    }
}

#[tokio::test]
async fn http_errors_are_diagnostic_retryable_and_secret_redacted() {
    let secret = "server-echoed-secret";
    let response = format!("{{\"error\":\"temporary failure for {secret}\"}}");
    let (endpoint, raw_request, server) = serve_once(503, "Service Unavailable", response).await;
    let client = OpenAiChatClient::with_timeout(
        model_config(endpoint),
        RuntimeSecretHeaders::bearer(SecretString::new(secret)).expect("valid secret header"),
        Duration::from_secs(2),
    )
    .expect("valid client");

    let error = client
        .complete(request())
        .await
        .expect_err("HTTP 503 should fail");

    assert!(matches!(
        error,
        ModelError::Http {
            status: 503,
            retryable: true,
            ..
        }
    ));
    let diagnostic = error.to_string();
    assert!(!diagnostic.contains(secret));
    assert!(diagnostic.contains("[REDACTED]"));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn html_gateway_errors_are_reduced_to_a_readable_title() {
    let response =
        "<html><head><title>504 Gateway Time-out</title></head><body>proxy</body></html>"
            .to_owned();
    let (endpoint, raw_request, server) = serve_once(504, "Gateway Time-out", response).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let error = client
        .complete(request())
        .await
        .expect_err("HTTP 504 should fail");

    assert!(matches!(
        error,
        ModelError::Http {
            status: 504,
            retryable: true,
            ..
        }
    ));
    assert!(error.to_string().contains("504 Gateway Time-out"));
    assert!(!error.to_string().contains("<html>"));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test]
async fn malformed_success_response_is_reported_as_invalid_response() {
    let (endpoint, raw_request, server) = serve_once(200, "OK", "{not valid JSON".to_owned()).await;
    let client = OpenAiChatClient::new(model_config(endpoint), RuntimeSecretHeaders::new())
        .expect("valid client");

    let error = client
        .complete(request())
        .await
        .expect_err("invalid JSON should fail");

    assert!(matches!(error, ModelError::InvalidResponse { .. }));
    assert!(error.to_string().contains("Chat Completions JSON"));
    let _ = raw_request.await.expect("captured HTTP request");
    server.await.expect("mock server should finish");
}

#[tokio::test(start_paused = true)]
async fn openai_http_timeout_is_diagnostic_without_wall_clock_sleep() {
    let (endpoint, accepted, release, server) = hold_one_request().await;
    let client = OpenAiChatClient::with_timeout(
        model_config(endpoint),
        RuntimeSecretHeaders::new(),
        Duration::from_secs(5),
    )
    .expect("valid client");
    let task = tokio::spawn(async move { client.complete(request()).await });

    accepted.await.expect("mock server should receive request");
    tokio::time::advance(Duration::from_secs(6)).await;
    let error = task
        .await
        .expect("client task should join")
        .expect_err("request should time out");

    assert!(matches!(error, ModelError::Timeout { timeout_ms: 5_000 }));
    let _ = release.send(());
    server.await.expect("mock server should finish");
}

#[tokio::test(start_paused = true)]
async fn openai_response_body_timeout_maps_to_timeout() {
    let (endpoint, headers_sent, release, server) = hold_response_body().await;
    let client = OpenAiChatClient::with_timeout(
        model_config(endpoint),
        RuntimeSecretHeaders::new(),
        Duration::from_secs(5),
    )
    .expect("valid client");
    let task = tokio::spawn(async move { client.complete(request()).await });

    headers_sent
        .await
        .expect("mock server should send response headers");
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(6)).await;
    let error = task
        .await
        .expect("client task should join")
        .expect_err("response body should time out");

    assert!(matches!(error, ModelError::Timeout { timeout_ms: 5_000 }));
    let _ = release.send(());
    server.await.expect("mock server should finish");
}
