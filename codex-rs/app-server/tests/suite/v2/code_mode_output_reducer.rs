use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

const READ_TIMEOUT: Duration = Duration::from_secs(60);
const REDUCE_PATH: &str = "/v1/reduce-code-mode-output";
const ACCEPT_PATH: &str = "/v1/accept-code-mode-output";
const NOISE_LINE: &str = "nested code mode output that should be reduced";

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

fn code_mode_override(value: Value) -> Option<HashMap<String, Value>> {
    Some(HashMap::from([("features.code_mode".to_string(), value)]))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn thread_start_and_loaded_resume_refresh_reducer_without_changing_code_mode_enablement()
-> Result<()> {
    let model_server = responses::start_mock_server().await;
    let responses_mock = responses::mount_sse_sequence(
        &model_server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("resp-1"),
                responses::ev_custom_tool_call(
                    "call-1",
                    "exec",
                    &format!("text('{NOISE_LINE}\\n'.repeat(200))"),
                ),
                responses::ev_completed("resp-1"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-1", "Done"),
                responses::ev_completed("resp-2"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-3"),
                responses::ev_custom_tool_call(
                    "call-2",
                    "exec",
                    &format!("text('{NOISE_LINE}\\n'.repeat(200))"),
                ),
                responses::ev_completed("resp-3"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-2", "Done"),
                responses::ev_completed("resp-4"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("resp-5"),
                responses::ev_custom_tool_call(
                    "call-3",
                    "exec",
                    &format!("text('{NOISE_LINE}\\n'.repeat(200))"),
                ),
                responses::ev_completed("resp-5"),
            ]),
            responses::sse(vec![
                responses::ev_assistant_message("msg-3", "Done"),
                responses::ev_completed("resp-6"),
            ]),
        ],
    )
    .await;

    let reducer = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path(REDUCE_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "response_id": "gate-app-server-1",
            "replacement": [{
                "type": "input_text",
                "text": "reducer replacement",
            }]
        })))
        .expect(1)
        .mount(&reducer)
        .await;
    Mock::given(method("POST"))
        .and(path(ACCEPT_PATH))
        .respond_with(ResponseTemplate::new(204))
        .expect(1)
        .mount(&reducer)
        .await;
    let codex_home = TempDir::new()?;
    let descriptor_path = codex_home.path().join("reducer.json");
    std::fs::write(
        &descriptor_path,
        json!({
            "version": 2,
            "url": format!("{}{REDUCE_PATH}", reducer.uri()),
            "token": "integration-test-token",
            "acceptance_url": format!("{}{ACCEPT_PATH}", reducer.uri()),
        })
        .to_string(),
    )?;
    MockResponsesConfig::new(&model_server.uri())
        .with_model("test-gpt-5.1-codex")
        .enable_feature(Feature::CodeMode)
        .enable_feature(Feature::CodeModeOnly)
        .write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let started = app_server
        .start_thread(ThreadStartParams {
            config: code_mode_override(json!({
                "output_reducer": {
                    "descriptor_path": descriptor_path,
                    "min_trigger_bytes": 100,
                }
            })),
            ..Default::default()
        })
        .await?;
    timeout(
        READ_TIMEOUT,
        app_server.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "first turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let resume_request_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id.clone(),
            config: code_mode_override(json!({})),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse =
        timeout(READ_TIMEOUT, app_server.read_response(resume_request_id)).await??;
    timeout(
        READ_TIMEOUT,
        app_server.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "second turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let resume_request_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id.clone(),
            config: code_mode_override(json!({
                "max_output_tokens_ceiling": 100,
            })),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse =
        timeout(READ_TIMEOUT, app_server.read_response(resume_request_id)).await??;
    timeout(
        READ_TIMEOUT,
        app_server.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: started.thread.id,
            input: vec![UserInput::Text {
                text: "third turn".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        }),
    )
    .await??;

    let requests = responses_mock.requests();
    assert_eq!(requests.len(), 6);
    let reduced = output_text(&requests[1].custom_tool_call_output("call-1")["output"]);
    let unreduced = output_text(&requests[3].custom_tool_call_output("call-2")["output"]);
    let ceiling_limited = output_text(&requests[5].custom_tool_call_output("call-3")["output"]);
    assert!(reduced.contains("reducer replacement"));
    assert!(!reduced.contains(NOISE_LINE));
    assert!(unreduced.contains(NOISE_LINE));
    assert!(!unreduced.contains("reducer replacement"));
    assert!(ceiling_limited.contains(NOISE_LINE));
    assert!(!ceiling_limited.contains("reducer replacement"));
    assert!(ceiling_limited.len() < unreduced.len());

    Ok(())
}
