//! End-to-end coverage for the code-mode output reducer.
//!
//! The unit tests in `core/src/tools/code_mode/reducer_tests.rs` drive
//! `apply_output_reduction` directly with a hand-built reducer. That proves the
//! wire contract and every fallback, but it never exercises the path that
//! actually matters in production: config -> `CodeModeService::new` -> a live
//! reducer -> the bytes the model receives. That path fails *silently* — if the
//! HTTP client cannot be built, `CodeModeService` stores `None`, logs a warning,
//! and the feature is inert while every unit test still passes.
//!
//! These tests run a real code-mode turn against a mocked Responses API and a
//! stub reducer bridge, and assert on what the model was sent on the next turn.

#![allow(clippy::unwrap_used)]

use anyhow::Result;
use codex_core::config::CodeModeOutputReducerConfig;
use codex_features::Feature;
use core_test_support::responses;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_custom_tool_call;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::test_codex;
use serde_json::Value;
use std::time::Duration;
use tempfile::TempDir;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const REDUCE_PATH: &str = "/v1/reduce-code-mode-output";
const BRIDGE_TOKEN: &str = "integration-test-token";
/// Emitted by the script below; comfortably over the trigger threshold the test
/// configures, and recognizable in the model-visible output.
const NOISE_LINE: &str = "at codex::frame::deep::stack::trace::line";

/// Writes the descriptor a host would write, pointing at `server`.
fn write_descriptor(dir: &TempDir, server: &MockServer) -> std::path::PathBuf {
    let descriptor_path = dir.path().join("bridge.json");
    std::fs::write(
        &descriptor_path,
        serde_json::json!({
            "version": 1,
            "url": format!("{}{REDUCE_PATH}", server.uri()),
            "token": BRIDGE_TOKEN,
        })
        .to_string(),
    )
    .unwrap();
    descriptor_path
}

fn reducer_config(descriptor_path: std::path::PathBuf) -> CodeModeOutputReducerConfig {
    CodeModeOutputReducerConfig {
        descriptor_path,
        // Low enough that the script below clears it, high enough that Codex's
        // own status preamble does not.
        min_trigger_bytes: 512,
        max_request_bytes: 8 * 1024 * 1024,
        max_response_bytes: 64 * 1024,
        timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(2),
    }
}

/// A script whose output is large enough to trigger reduction.
fn noisy_script() -> String {
    format!("text('{NOISE_LINE}\\n'.repeat(200))")
}

/// Runs one code-mode turn and returns the serialized next request, which is
/// what the model was actually sent.
async fn run_turn_and_read_model_visible_output(
    reducer: Option<CodeModeOutputReducerConfig>,
) -> Result<String> {
    let server = responses::start_mock_server().await;
    // Mirrors run_code_mode_turn_with_model_and_config in code_mode.rs. This
    // model's catalog entry already selects code mode; enabling the feature
    // alone leaves `exec` unregistered and every turn answers
    // "unsupported custom tool call: exec".
    let mut builder = test_codex()
        .with_model("test-gpt-5.1-codex")
        .with_config(move |config| {
            let _ = config.features.enable(Feature::CodeMode);
            let _ = config.features.enable(Feature::ExecutedToolCallMetadata);
            config.code_mode.output_reducer = reducer;
        });
    let test = builder.build(&server).await?;

    responses::mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("resp-1"),
            ev_custom_tool_call("call-1", "exec", &noisy_script()),
            ev_completed("resp-1"),
        ]),
    )
    .await;
    let follow_up = responses::mount_sse_once(
        &server,
        sse(vec![
            ev_assistant_message("msg-1", "done"),
            ev_completed("resp-2"),
        ]),
    )
    .await;

    test.submit_turn("summarize the output").await?;

    Ok(model_visible_tool_output(&follow_up.single_request().body_json()))
}

/// Flattens a tool output, which is a bare string on the error path but an
/// array of content items when code mode actually ran.
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

/// Pulls out only the `custom_tool_call_output` for `call-1`.
///
/// Asserting on the whole serialized request looks robust and is not: the
/// request also carries the `custom_tool_call` *input*, which is the script
/// source, which contains the very text these tests look for. Two of these
/// tests passed that way while `exec` was not even registered.
fn model_visible_tool_output(body: &Value) -> String {
    let outputs: Vec<String> = body["input"]
        .as_array()
        .expect("request should carry input items")
        .iter()
        .filter(|item| {
            item["type"] == "custom_tool_call_output" && item["call_id"] == "call-1"
        })
        .map(|item| output_text(&item["output"]))
        .collect();
    assert!(
        !outputs.is_empty(),
        "no tool output for call-1 in: {body}"
    );
    let joined = outputs.concat();
    assert!(
        !joined.is_empty(),
        "tool output was present but extracted empty, so the shape changed: {body}"
    );
    // Usually means codex-code-mode-host was not built, so
    // CodeModeSessionProvider::availability failed and code mode fell back to
    // direct tools. Fail loudly: a session where the seam was never reached
    // must not read as a pass.
    assert!(
        !joined.contains("unsupported custom tool call"),
        "code mode was not active, so nothing under test actually ran \
         (is codex-code-mode-host built?): {joined}"
    );
    joined
}

/// With no reducer configured the model sees the script's own output, only ever
/// shortened by the built-in truncation.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_a_reducer_the_model_sees_the_script_output() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let output = run_turn_and_read_model_visible_output(None).await?;

    assert!(
        output.contains(NOISE_LINE),
        "unreduced output should reach the model: {output}"
    );
    assert!(
        !output.contains("untrusted_reduced_output"),
        "nothing should be fenced when no reducer is configured: {output}"
    );
    Ok(())
}

/// The whole point: a configured reducer replaces what the model reads, the
/// replacement is fenced as untrusted, and the original never arrives.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_configured_reducer_replaces_what_the_model_reads() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let bridge = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(REDUCE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "replacement": [{
                "type": "input_text",
                "text": "200 identical stack frames; full output preserved as tm-1.",
            }]
        })))
        .mount(&bridge)
        .await;
    let dir = TempDir::new()?;
    let descriptor_path = write_descriptor(&dir, &bridge);

    let output = run_turn_and_read_model_visible_output(Some(reducer_config(descriptor_path)))
        .await?;

    assert!(
        output.contains("200 identical stack frames"),
        "the replacement should reach the model: {output}"
    );
    assert!(
        output.contains("untrusted_reduced_output"),
        "the replacement should be fenced as untrusted data: {output}"
    );
    assert!(
        !output.contains(NOISE_LINE),
        "the original output must not reach the model: {output}"
    );

    // The bridge must have been told what produced the output, not just handed
    // an anonymous blob.
    let requests = bridge.received_requests().await.expect("recorded requests");
    assert_eq!(requests.len(), 1, "exactly one reduction per cell");
    let body: Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(body["version"], serde_json::json!(1));
    assert!(
        body["script"]
            .as_str()
            .is_some_and(|script| script.contains(NOISE_LINE)),
        "the reducer should receive the script that produced the output: {body}"
    );
    assert!(
        body["content_items"]
            .as_array()
            .is_some_and(|items| !items.is_empty()),
        "the reducer should receive the full original: {body}"
    );
    assert!(
        !body["cell_id"].as_str().unwrap_or_default().is_empty(),
        "the reducer needs a cell id to file the preserved output: {body}"
    );
    Ok(())
}

/// A dead bridge must degrade to the built-in truncation rather than failing the
/// turn. This is the property that lets an operator enable the feature without
/// making Codex depend on their host being up.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_unreachable_reducer_leaves_the_turn_working() -> Result<()> {
    skip_if_no_network!(Ok(()));

    // A descriptor pointing at a bridge that was never started.
    let dir = TempDir::new()?;
    let descriptor_path = dir.path().join("bridge.json");
    std::fs::write(
        &descriptor_path,
        serde_json::json!({
            "version": 1,
            // Port 1 is reserved and never listening.
            "url": format!("http://127.0.0.1:1{REDUCE_PATH}"),
            "token": BRIDGE_TOKEN,
        })
        .to_string(),
    )?;

    let output = run_turn_and_read_model_visible_output(Some(reducer_config(descriptor_path)))
        .await?;

    assert!(
        output.contains(NOISE_LINE),
        "an unreachable reducer must fall back to the original output: {output}"
    );
    assert!(
        !output.contains("untrusted_reduced_output"),
        "nothing should be fenced when the reducer never answered: {output}"
    );
    Ok(())
}
