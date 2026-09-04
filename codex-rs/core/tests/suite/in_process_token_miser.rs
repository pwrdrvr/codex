//! End-to-end coverage for Codex-owned Token Miser reduction.

#![allow(clippy::unwrap_used)]

use anyhow::Context;
use anyhow::Result;
use codex_core::TurnInputRequest;
use codex_core::config::CodeModeOutputReducerConfig;
use codex_core::config::DEFAULT_CODE_MODE_REDUCER_TOOL_DESCRIPTION_GUIDANCE;
use codex_core::config::InProcessTokenMiserConfig;
use codex_core::test_support::code_mode;
use codex_features::Feature;
use codex_history::RolloutItem;
use codex_history::TokenMiserStoredOutcome;
use codex_protocol::models::FunctionCallOutputContentItem as ProtocolFunctionCallOutputContentItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::user_input::UserInput;
use codex_thread_store::LoadThreadHistoryParams;
use codex_utils_output_truncation::approx_token_count;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::TestCodexBuilder;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const RAW_SECRET: &str = "raw-terminal-secret-that-only-luna-may-release";
const RAW_HEAD: &str = "raw-terminal";
const RAW_TAIL: &str = "-may-release";
const HIDDEN_REASONING: &str = "private parent reasoning must never reach token miser";
const DEFERRED_SCRIPT: &str = "deferred-token-miser-result";
const LIVE_PREVIEW: &str = "bounded live preview";
const LARGE_SCRIPT: &str = "large-token-miser-result";
const LARGE_SUFFIX_BYTES: usize = 200_000;

struct TokenMiserCodeModeProvider;

impl code_mode::CodeModeSessionProvider for TokenMiserCodeModeProvider {
    fn create_session<'a>(
        &'a self,
        delegate: Arc<dyn code_mode::CodeModeSessionDelegate>,
    ) -> code_mode::CodeModeSessionProviderFuture<'a> {
        Box::pin(async move {
            Ok(Arc::new(TokenMiserCodeModeSession {
                delegate,
                next_cell: AtomicUsize::new(1),
            }) as Arc<dyn code_mode::CodeModeSession>)
        })
    }
}

struct TokenMiserCodeModeSession {
    delegate: Arc<dyn code_mode::CodeModeSessionDelegate>,
    next_cell: AtomicUsize,
}

impl code_mode::CodeModeSession for TokenMiserCodeModeSession {
    fn execute<'a>(
        &'a self,
        request: code_mode::ExecuteRequest,
    ) -> code_mode::CodeModeSessionResultFuture<'a, code_mode::StartedCell> {
        let cell_id = code_mode::CellId::new(format!(
            "token-miser-test-cell-{}",
            self.next_cell.fetch_add(1, Ordering::SeqCst)
        ));
        let response_cell_id = cell_id.clone();
        let delegate = Arc::clone(&self.delegate);
        Box::pin(async move {
            Ok(code_mode::StartedCell::from_future(cell_id, async move {
                if request.source == DEFERRED_SCRIPT {
                    return Ok(code_mode::RuntimeResponse::Yielded {
                        cell_id: response_cell_id,
                        content_items: vec![code_mode::FunctionCallOutputContentItem::InputText {
                            text: LIVE_PREVIEW.to_string(),
                        }],
                    });
                }
                let text = if let Some(object_id) = retrieval_object_id(&request.source) {
                    delegate
                        .invoke_tool(
                            code_mode::CodeModeNestedToolCall {
                                cell_id: response_cell_id.clone(),
                                runtime_tool_call_id: "token-miser-retrieval".to_string(),
                                tool_name: codex_protocol::ToolName::plain(
                                    "read_token_miser_output",
                                ),
                                tool_kind: code_mode::CodeModeToolKind::Function,
                                input: Some(serde_json::json!({
                                    "object_id": object_id,
                                    "item_index": 0,
                                    "offset": 0,
                                    "max_bytes": 4096,
                                })),
                            },
                            CancellationToken::new(),
                        )
                        .await?
                        .to_string()
                } else if request.source == LARGE_SCRIPT {
                    format!("{RAW_SECRET}{}", "x".repeat(LARGE_SUFFIX_BYTES))
                } else {
                    RAW_SECRET.to_string()
                };
                delegate.cell_closed(&response_cell_id);
                Ok(code_mode::RuntimeResponse::Result {
                    cell_id: response_cell_id,
                    content_items: vec![code_mode::FunctionCallOutputContentItem::InputText {
                        text,
                    }],
                    error_text: None,
                })
            }))
        })
    }

    fn wait<'a>(
        &'a self,
        request: code_mode::WaitRequest,
    ) -> code_mode::CodeModeSessionResultFuture<'a, code_mode::WaitOutcome> {
        let delegate = Arc::clone(&self.delegate);
        Box::pin(async move {
            delegate.cell_closed(&request.cell_id);
            Ok(code_mode::WaitOutcome::LiveCell(
                code_mode::RuntimeResponse::Result {
                    cell_id: request.cell_id,
                    content_items: vec![code_mode::FunctionCallOutputContentItem::InputText {
                        text: RAW_SECRET.to_string(),
                    }],
                    error_text: None,
                },
            ))
        })
    }

    fn terminate<'a>(
        &'a self,
        cell_id: code_mode::CellId,
    ) -> code_mode::CodeModeSessionResultFuture<'a, code_mode::WaitOutcome> {
        Box::pin(async move {
            Ok(code_mode::WaitOutcome::MissingCell(
                code_mode::RuntimeResponse::Terminated {
                    cell_id,
                    content_items: Vec::new(),
                },
            ))
        })
    }

    fn shutdown<'a>(&'a self) -> code_mode::CodeModeSessionResultFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}

fn retrieval_object_id(source: &str) -> Option<String> {
    let marker = "object_id: \"";
    let start = source.find(marker)? + marker.len();
    let end = source[start..].find('"')? + start;
    Some(source[start..end].to_string())
}

fn is_captured_token_miser_request(request: &responses::ResponsesRequest) -> bool {
    request.header("x-openai-subagent").as_deref() == Some("token_miser")
        || request.body_json()["client_metadata"]["x-openai-subagent"].as_str()
            == Some("token_miser")
}

fn token_miser_config() -> InProcessTokenMiserConfig {
    InProcessTokenMiserConfig {
        model: "gpt-5.6-luna".to_string(),
        timeout: Duration::from_secs(10),
        max_reducer_input_bytes: 128 * 1024,
        max_replacement_bytes: 4 * 1024,
    }
}

fn token_miser_test_builder() -> TestCodexBuilder {
    test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_code_mode_session_provider(Arc::new(TokenMiserCodeModeProvider))
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config
                .features
                .enable(Feature::ExecutedToolCallMetadata)
                .unwrap();
            config.code_mode.token_miser = Some(token_miser_config());
            config.code_mode.output_reducer = Some(CodeModeOutputReducerConfig {
                descriptor_path: PathBuf::from("must-not-run-managed-token-miser.json"),
                min_trigger_bytes: 0,
                max_request_bytes: 1_024,
                max_response_bytes: 1_024,
                timeout: Duration::from_millis(1),
                connect_timeout: Duration::from_millis(1),
                tool_description_guidance: DEFAULT_CODE_MODE_REDUCER_TOOL_DESCRIPTION_GUIDANCE
                    .to_string(),
                continuation_guidance: None,
            });
        })
}

fn ev_completed_with_usage(
    id: &str,
    input_tokens: i64,
    cached_input_tokens: i64,
    cache_write_input_tokens: i64,
    output_tokens: i64,
    reasoning_output_tokens: i64,
    total_tokens: i64,
) -> Value {
    serde_json::json!({
        "type": "response.completed",
        "response": {
            "id": id,
            "usage": {
                "input_tokens": input_tokens,
                "input_tokens_details": {
                    "cached_tokens": cached_input_tokens,
                    "cache_write_tokens": cache_write_input_tokens,
                },
                "output_tokens": output_tokens,
                "output_tokens_details": { "reasoning_tokens": reasoning_output_tokens },
                "total_tokens": total_tokens,
            }
        }
    })
}

fn output_text(output: &Value) -> String {
    match output {
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| item["text"].as_str())
            .collect::<Vec<_>>()
            .join(""),
        Value::Object(_) => output_text(&output["content"]),
        other => other.to_string(),
    }
}

fn find_string_containing<'a>(value: &'a Value, needle: &str) -> Option<&'a str> {
    match value {
        Value::String(text) => text.contains(needle).then_some(text),
        Value::Array(values) => values
            .iter()
            .find_map(|value| find_string_containing(value, needle)),
        Value::Object(values) => values
            .values()
            .find_map(|value| find_string_containing(value, needle)),
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

fn assert_luna_received_only_the_bounded_raw_view(body: &Value) {
    let serialized = body.to_string();
    assert!(serialized.contains(RAW_HEAD));
    assert!(serialized.contains(RAW_TAIL));
    assert!(!serialized.contains(RAW_SECRET));
}

fn tool_output_for_call(body: &Value, call_id: &str) -> String {
    body["input"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|item| {
            (item["type"] == "custom_tool_call_output" && item["call_id"] == call_id)
                .then(|| output_text(&item["output"]))
        })
        .expect("model request should contain the Code Mode result")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn immediate_replacement_takes_precedence_over_managed_reducer_without_reasoning_leakage()
-> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("parent-1"),
                ev_assistant_message("visible-intent", "Inspect the command result."),
                ev_reasoning_item(
                    "private-reasoning",
                    &[HIDDEN_REASONING],
                    &[HIDDEN_REASONING],
                ),
                ev_custom_tool_call("exec-1", "exec", "text('produce retained output')"),
                ev_completed_with_usage("parent-1", 11, 2, 3, 5, 4, 16),
            ]),
            sse(vec![
                ev_response_created("luna-1"),
                ev_assistant_message(
                    "luna-answer",
                    r#"{"decision":"replace","replacement":"selected fact"}"#,
                ),
                ev_completed_with_usage("luna-1", 7, 1, 2, 6, 3, 13),
            ]),
            sse(vec![
                ev_response_created("parent-2"),
                ev_assistant_message("done", "done"),
                ev_completed("parent-2"),
            ]),
        ],
    )
    .await;

    let test = token_miser_test_builder()
        .build_with_auto_env(&server)
        .await?;

    test.submit_turn("reduce the command output").await?;

    let requests = exchange.requests();
    assert_eq!(requests.len(), 3);
    assert!(is_captured_token_miser_request(&requests[1]));
    let luna_body = requests[1].body_json();
    let serialized_luna_body = luna_body.to_string();
    assert_eq!(luna_body["tools"], serde_json::json!([]));
    let framed_input = find_string_containing(&luna_body, "<token_miser_input>")
        .expect("Luna request should contain one framed Token Miser input");
    assert!(framed_input.len() <= 896);
    assert!(approx_token_count(framed_input) < 1_000);
    assert_luna_received_only_the_bounded_raw_view(&luna_body);
    assert!(!serialized_luna_body.contains(HIDDEN_REASONING));
    assert!(!serialized_luna_body.contains("Inspect the command result."));

    let visible = tool_output_for_call(&requests[2].body_json(), "exec-1");
    assert!(visible.contains("selected fact"));
    assert!(!visible.contains(RAW_SECRET));

    let history = test
        .thread_store
        .load_history(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserOutput(_)))
            .count(),
        1
    );
    let decisions = history
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::TokenMiserDecision(decision) => Some(decision),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].usage,
        Some(TokenUsage {
            input_tokens: 7,
            cached_input_tokens: 1,
            cache_write_input_tokens: 2,
            output_tokens: 6,
            reasoning_output_tokens: 3,
            total_tokens: 13,
            codex_rollout_budget_units: None,
        })
    );
    let total = history.items.iter().rev().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::TokenCount(event)) => event
            .info
            .as_ref()
            .map(|info| info.total_token_usage.clone()),
        _ => None,
    });
    assert_eq!(
        total,
        Some(TokenUsage {
            input_tokens: 18,
            cached_input_tokens: 3,
            cache_write_input_tokens: 5,
            output_tokens: 11,
            reasoning_output_tokens: 7,
            total_tokens: 29,
            codex_rollout_budget_units: None,
        })
    );

    let object_id = visible
        .split("Exact output object: ")
        .nth(1)
        .and_then(|rest| rest.split('.').next())
        .expect("receipt should expose an object id")
        .to_string();
    let retrieval_prompt = "retrieve the exact retained item after resume";
    let retrieval_script = format!(
        "const result = await tools.read_token_miser_output({{ object_id: {object_id:?}, item_index: 0, offset: 0, max_bytes: 4096 }}); text(JSON.stringify(result));"
    );
    let retrieval_exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("parent-retrieval-1"),
                ev_custom_tool_call("retrieve-exec", "exec", &retrieval_script),
                ev_completed("parent-retrieval-1"),
            ]),
            sse(vec![
                ev_response_created("parent-retrieval-2"),
                ev_assistant_message("retrieval-done", "done"),
                ev_completed("parent-retrieval-2"),
            ]),
        ],
    )
    .await;
    let mut resumed_builder = token_miser_test_builder();
    let resumed = resumed_builder.restart(&server, &test).await?;

    resumed.submit_turn(retrieval_prompt).await?;

    let retrieval_requests = retrieval_exchange.requests();
    assert_eq!(retrieval_requests.len(), 2);
    assert!(
        retrieval_requests[0]
            .body_json()
            .to_string()
            .contains(retrieval_prompt)
    );
    assert!(
        !retrieval_requests[0]
            .body_json()
            .to_string()
            .contains(RAW_SECRET)
    );
    let retrieval_request = &retrieval_requests[1];
    let retrieval_visible = tool_output_for_call(&retrieval_request.body_json(), "retrieve-exec");
    assert!(
        retrieval_visible.contains(RAW_SECRET),
        "retrieval output: {retrieval_visible}"
    );
    let resumed_history = resumed
        .thread_store
        .load_history(LoadThreadHistoryParams {
            thread_id: resumed.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    assert_eq!(
        resumed_history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserDecision(_)))
            .count(),
        1
    );
    let resumed_total = resumed_history
        .items
        .iter()
        .rev()
        .find_map(|item| match item {
            RolloutItem::EventMsg(EventMsg::TokenCount(event)) => event
                .info
                .as_ref()
                .map(|info| info.total_token_usage.clone()),
            _ => None,
        });
    assert_eq!(resumed_total, total);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn direct_small_large_passthrough_and_failure_outputs_remain_exact() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for (case, script, reducer_answer, expect_raw_visible, expected_bytes) in [
        (
            "passthrough",
            "text('fixture output')",
            r#"{"decision":"passthrough","replacement":null}"#,
            true,
            RAW_SECRET.len(),
        ),
        (
            "malformed",
            "text('fixture output')",
            "not valid reducer json",
            false,
            RAW_SECRET.len(),
        ),
        (
            "large",
            LARGE_SCRIPT,
            r#"{"decision":"replace","replacement":"bounded large result"}"#,
            false,
            RAW_SECRET.len() + LARGE_SUFFIX_BYTES,
        ),
    ] {
        let server = responses::start_mock_server().await;
        let exchange = responses::mount_sse_sequence(
            &server,
            vec![
                sse(vec![
                    ev_response_created(&format!("parent-{case}")),
                    ev_custom_tool_call("exec-case", "exec", script),
                    ev_completed(&format!("parent-{case}")),
                ]),
                sse(vec![
                    ev_response_created(&format!("luna-{case}")),
                    ev_assistant_message(&format!("answer-{case}"), reducer_answer),
                    ev_completed_with_usage(&format!("luna-{case}"), 2, 1, 1, 1, 0, 3),
                ]),
                sse(vec![
                    ev_response_created(&format!("done-{case}")),
                    ev_assistant_message(&format!("message-{case}"), "done"),
                    ev_completed(&format!("done-{case}")),
                ]),
            ],
        )
        .await;
        let test = token_miser_test_builder()
            .build_with_auto_env(&server)
            .await?;

        test.submit_turn(&format!("run the {case} case")).await?;

        let requests = exchange.requests();
        assert_eq!(requests.len(), 3);
        assert!(is_captured_token_miser_request(&requests[1]));
        let luna_body = requests[1].body_json();
        let framed_input = find_string_containing(&luna_body, "<token_miser_input>")
            .expect("Luna request should contain one framed Token Miser input");
        assert!(framed_input.len() <= 896);
        let visible = tool_output_for_call(&requests[2].body_json(), "exec-case");
        assert_eq!(visible.contains(RAW_SECRET), expect_raw_visible);
        assert!(visible.contains("Exact output object:"));
        let history = test
            .thread_store
            .load_history(LoadThreadHistoryParams {
                thread_id: test.session_configured.thread_id,
                include_archived: true,
            })
            .await?;
        let outputs = history
            .items
            .iter()
            .filter_map(|item| match item {
                RolloutItem::TokenMiserOutput(output) => Some(output),
                _ => None,
            })
            .collect::<Vec<_>>();
        let outcomes = history
            .items
            .iter()
            .filter_map(|item| match item {
                RolloutItem::TokenMiserDecision(decision) => Some(&decision.outcome),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(outputs.len(), 1);
        let ProtocolFunctionCallOutputContentItem::InputText { text } =
            &outputs[0].content_items[0]
        else {
            panic!("expected exact persisted text output");
        };
        assert_eq!(text.len(), expected_bytes);
        assert_eq!(outcomes.len(), 1);
        assert_eq!(
            matches!(outcomes[0], TokenMiserStoredOutcome::Passthrough),
            expect_raw_visible
        );
    }

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn yielded_exec_is_reduced_once_only_after_terminal_wait() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("deferred-parent-1"),
                ev_custom_tool_call("exec-deferred", "exec", DEFERRED_SCRIPT),
                ev_completed("deferred-parent-1"),
            ]),
            sse(vec![
                ev_response_created("deferred-parent-2"),
                ev_assistant_message("deferred-waiting", "waiting"),
                ev_completed("deferred-parent-2"),
            ]),
            sse(vec![
                ev_response_created("deferred-parent-3"),
                ev_function_call(
                    "wait-deferred",
                    "wait",
                    &serde_json::to_string(&serde_json::json!({
                        "cell_id": "token-miser-test-cell-1",
                        "yield_time_ms": 1,
                    }))?,
                ),
                ev_completed("deferred-parent-3"),
            ]),
            sse(vec![
                ev_response_created("deferred-luna"),
                ev_assistant_message(
                    "deferred-luna-answer",
                    r#"{"decision":"replace","replacement":"deferred selected fact"}"#,
                ),
                ev_completed_with_usage("deferred-luna", 2, 0, 0, 1, 0, 3),
            ]),
            sse(vec![
                ev_response_created("deferred-parent-4"),
                ev_assistant_message("deferred-done", "done"),
                ev_completed("deferred-parent-4"),
            ]),
        ],
    )
    .await;
    let test = token_miser_test_builder()
        .build_with_auto_env(&server)
        .await?;

    test.submit_turn("start a deferred result").await?;
    let first_requests = exchange.requests();
    assert_eq!(first_requests.len(), 2);
    let live = tool_output_for_call(&first_requests[1].body_json(), "exec-deferred");
    assert!(!live.contains(LIVE_PREVIEW));
    assert!(live.contains("Script running with cell ID token-miser-test-cell-1"));
    assert!(!live.contains("Exact output object:"));

    test.submit_turn("collect the deferred result").await?;

    let requests = exchange.requests();
    assert_eq!(requests.len(), 5);
    assert!(is_captured_token_miser_request(&requests[3]));
    assert_luna_received_only_the_bounded_raw_view(&requests[3].body_json());
    let terminal = output_text(&requests[4].function_call_output("wait-deferred")["output"]);
    assert!(terminal.contains("deferred selected fact"));
    assert!(!terminal.contains(RAW_SECRET));
    let history = test
        .thread_store
        .load_history(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserOutput(_)))
            .count(),
        1
    );
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserDecision(_)))
            .count(),
        1
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn parallel_terminals_have_unique_objects_and_exact_once_usage() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    let luna_response = |id: &str| {
        sse(vec![
            ev_response_created(id),
            ev_assistant_message(
                &format!("{id}-answer"),
                r#"{"decision":"replace","replacement":"parallel fact"}"#,
            ),
            ev_completed_with_usage(id, 2, 1, 1, 1, 1, 3),
        ])
    };
    let exchange = responses::mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("parallel-parent-1"),
                ev_custom_tool_call("exec-a", "exec", "text('a')"),
                ev_custom_tool_call("exec-b", "exec", "text('b')"),
                ev_completed_with_usage("parallel-parent-1", 10, 2, 2, 4, 2, 14),
            ]),
            luna_response("parallel-luna-1"),
            luna_response("parallel-luna-2"),
            sse(vec![
                ev_response_created("parallel-parent-2"),
                ev_assistant_message("parallel-done", "done"),
                ev_completed("parallel-parent-2"),
            ]),
        ],
    )
    .await;
    let test = token_miser_test_builder()
        .build_with_auto_env(&server)
        .await?;

    test.submit_turn("run independent outputs").await?;

    let requests = exchange.requests();
    assert_eq!(requests.len(), 4);
    assert!(requests[1..3].iter().all(is_captured_token_miser_request));
    for call_id in ["exec-a", "exec-b"] {
        let visible = tool_output_for_call(&requests[3].body_json(), call_id);
        assert!(visible.contains("parallel fact"));
        assert!(!visible.contains(RAW_SECRET));
    }
    let history = test
        .thread_store
        .load_history(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    let objects = history
        .items
        .iter()
        .filter_map(|item| match item {
            RolloutItem::TokenMiserOutput(output) => Some(&output.object_id),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(objects.len(), 2);
    assert_ne!(objects[0], objects[1]);
    assert!(
        objects
            .iter()
            .all(|object_id| Uuid::parse_str(object_id).is_ok())
    );
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserDecision(_)))
            .count(),
        2
    );
    let total = history.items.iter().rev().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::TokenCount(event)) => event
            .info
            .as_ref()
            .map(|info| info.total_token_usage.clone()),
        _ => None,
    });
    assert_eq!(
        total,
        Some(TokenUsage {
            input_tokens: 14,
            cached_input_tokens: 4,
            cache_write_input_tokens: 4,
            output_tokens: 6,
            reasoning_output_tokens: 4,
            total_tokens: 20,
            codex_rollout_budget_units: None,
        })
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_does_not_drop_or_repeat_reducer_accounting() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            serde_json::from_slice::<Value>(&request.body)
                .expect("Responses request body should be valid JSON")
                .pointer("/client_metadata/x-openai-subagent")
                .and_then(Value::as_str)
                != Some("token_miser")
        },
        responses::sse_response(sse(vec![
            ev_response_created("cancel-parent"),
            ev_custom_tool_call("exec-cancel", "exec", "text('cancel fixture')"),
            ev_completed("cancel-parent"),
        ])),
    )
    .await;
    let delayed_luna = responses::mount_response_once_match(
        &server,
        |request: &wiremock::Request| {
            serde_json::from_slice::<Value>(&request.body)
                .expect("Responses request body should be valid JSON")
                .pointer("/client_metadata/x-openai-subagent")
                .and_then(Value::as_str)
                == Some("token_miser")
        },
        responses::sse_response(sse(vec![
            ev_response_created("cancel-luna"),
            ev_assistant_message(
                "cancel-luna-answer",
                r#"{"decision":"replace","replacement":"cancelled turn fact"}"#,
            ),
            ev_completed_with_usage("cancel-luna", 2, 1, 1, 1, 1, 3),
        ]))
        .set_delay(Duration::from_millis(250)),
    )
    .await;
    let test = token_miser_test_builder()
        .build_with_auto_env(&server)
        .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "start a reducer and interrupt its parent".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    tokio::time::timeout(Duration::from_secs(5), async {
        while delayed_luna.requests().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("Token Miser reducer did not start")?;
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    wait_for_event(&test.codex, |event| {
        matches!(
            event,
            EventMsg::TokenCount(event)
                if event.info.as_ref().is_some_and(|info| info.total_token_usage.total_tokens == 3)
        )
    })
    .await;

    let history = test
        .thread_store
        .load_history(LoadThreadHistoryParams {
            thread_id: test.session_configured.thread_id,
            include_archived: true,
        })
        .await?;
    assert_eq!(delayed_luna.requests().len(), 1);
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserOutput(_)))
            .count(),
        1
    );
    assert_eq!(
        history
            .items
            .iter()
            .filter(|item| matches!(item, RolloutItem::TokenMiserDecision(_)))
            .count(),
        1
    );
    let total = history.items.iter().rev().find_map(|item| match item {
        RolloutItem::EventMsg(EventMsg::TokenCount(event)) => event
            .info
            .as_ref()
            .map(|info| info.total_token_usage.clone()),
        _ => None,
    });
    assert_eq!(
        total,
        Some(TokenUsage {
            input_tokens: 2,
            cached_input_tokens: 1,
            cache_write_input_tokens: 1,
            output_tokens: 1,
            reasoning_output_tokens: 1,
            total_tokens: 3,
            codex_rollout_budget_units: None,
        })
    );

    Ok(())
}
