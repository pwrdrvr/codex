//! End-to-end coverage for Codex-owned Token Miser reduction.

#![allow(clippy::unwrap_used)]

use anyhow::Result;
use codex_core::config::InProcessTokenMiserConfig;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_completed_with_tokens;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_reasoning_item;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once_match;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use std::time::Duration;

const RAW_SECRET: &str = "raw-terminal-secret-that-only-luna-may-release";
const HIDDEN_REASONING: &str = "private parent reasoning must never reach token miser";

fn wire_request_contains(request: &wiremock::Request, text: &str) -> bool {
    std::str::from_utf8(&request.body).is_ok_and(|body| body.contains(text))
}

fn token_miser_config() -> InProcessTokenMiserConfig {
    InProcessTokenMiserConfig {
        model: "gpt-5.6-luna".to_string(),
        timeout: Duration::from_secs(10),
        max_reducer_input_bytes: 128 * 1024,
        max_replacement_bytes: 4 * 1024,
    }
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
async fn immediate_result_is_replaced_by_luna_without_reasoning_leakage() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = responses::start_mock_server().await;
    mount_sse_once_match(
        &server,
        |request: &wiremock::Request| wire_request_contains(request, "reduce the command output"),
        sse(vec![
            ev_response_created("parent-1"),
            ev_assistant_message("visible-intent", "Inspect the command result."),
            ev_reasoning_item("private-reasoning", &[HIDDEN_REASONING], &[HIDDEN_REASONING]),
            ev_custom_tool_call(
                "exec-1",
                "exec",
                &format!("text({RAW_SECRET:?})"),
            ),
            ev_completed_with_tokens("parent-1", 11),
        ]),
    )
    .await;
    let luna = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| {
            wire_request_contains(request, "\"x-openai-subagent\":\"token_miser\"")
        },
        sse(vec![
            ev_response_created("luna-1"),
            ev_assistant_message(
                "luna-answer",
                r#"{"decision":"replace","replacement":"selected fact"}"#,
            ),
            ev_completed_with_tokens("luna-1", 7),
        ]),
    )
    .await;
    let follow_up = mount_sse_once_match(
        &server,
        |request: &wiremock::Request| wire_request_contains(request, "selected fact"),
        sse(vec![
            ev_response_created("parent-2"),
            ev_assistant_message("done", "done"),
            ev_completed("parent-2"),
        ]),
    )
    .await;

    let test = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(|config| {
            config.features.enable(Feature::CodeMode).unwrap();
            config.code_mode.token_miser = Some(token_miser_config());
        })
        .build_with_auto_env(&server)
        .await?;

    test.submit_turn("reduce the command output").await?;

    let luna_body = luna.single_request().body_json();
    let serialized_luna_body = luna_body.to_string();
    assert!(serialized_luna_body.contains(RAW_SECRET));
    assert!(!serialized_luna_body.contains(HIDDEN_REASONING));

    let visible = tool_output_for_call(&follow_up.single_request().body_json(), "exec-1");
    assert!(visible.contains("selected fact"));
    assert!(!visible.contains(RAW_SECRET));

    Ok(())
}
