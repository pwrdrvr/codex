use std::collections::HashMap;
use std::time::Duration;

use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::DynamicToolFunctionSpec;
use codex_app_server_protocol::DynamicToolSpec;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
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
const RESUME_TOOL_GUIDANCE: &str = "resume-configured tool guidance";
const RESUME_CONTINUATION_GUIDANCE: &str = "resume-configured continuation guidance";

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
async fn loaded_resume_rejects_reducer_refresh_while_turn_is_running() -> Result<()> {
    let model_server = responses::start_mock_server().await;
    let delayed_response = responses::sse_response(responses::sse(vec![
        responses::ev_response_created("resp-running"),
        responses::ev_assistant_message("msg-running", "Done"),
        responses::ev_completed("resp-running"),
    ]))
    .set_delay(Duration::from_secs(2));
    let _response_mock = responses::mount_response_once(&model_server, delayed_response).await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&model_server.uri())
        .with_model("test-gpt-5.1-codex")
        .enable_feature(Feature::CodeMode)
        .write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let started = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let turn_request_id = app_server
        .send_turn_start_request(TurnStartParams {
            thread_id: started.thread.id.clone(),
            input: vec![UserInput::Text {
                text: "keep this turn active".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_response_message(RequestId::Integer(turn_request_id)),
    )
    .await??;
    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/started"),
    )
    .await??;

    let resume_request_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id,
            config: code_mode_override(json!({
                "max_output_tokens_ceiling": 64,
            })),
            ..Default::default()
        })
        .await?;
    let resume_error: JSONRPCError = timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_error_message(RequestId::Integer(resume_request_id)),
    )
    .await??;
    assert!(
        resume_error.error.message.contains("output reducer")
            && resume_error.error.message.contains("active turn"),
        "unexpected resume error: {}",
        resume_error.error.message
    );

    timeout(
        READ_TIMEOUT,
        app_server.read_stream_until_notification_message("turn/completed"),
    )
    .await??;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loaded_resume_before_first_turn_activates_reducer_and_dynamic_tools() -> Result<()> {
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
            "version": 1,
            "url": format!("{}{REDUCE_PATH}", reducer.uri()),
            "token": "integration-test-token",
            "acceptance_url": format!("{}{ACCEPT_PATH}", reducer.uri()),
        })
        .to_string(),
    )?;
    MockResponsesConfig::new(&model_server.uri())
        .with_model("test-gpt-5.1-codex")
        .enable_feature(Feature::CodeMode)
        .write(codex_home.path())?;

    let mut app_server = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build_initialized_with_timeout(READ_TIMEOUT)
        .await?;
    let started = app_server
        .start_thread(ThreadStartParams::default())
        .await?;
    let resume_request_id = app_server
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: started.thread.id.clone(),
            config: code_mode_override(json!({
                "output_reducer": {
                    "descriptor_path": descriptor_path,
                    "min_trigger_bytes": 100,
                    "tool_description_guidance": RESUME_TOOL_GUIDANCE,
                    "continuation_guidance": RESUME_CONTINUATION_GUIDANCE,
                }
            })),
            dynamic_tools: Some(vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "token_miser_search".to_string(),
                description: "Search preserved Token Miser output".to_string(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string" }
                    },
                    "required": ["query"],
                    "additionalProperties": false,
                }),
                defer_loading: false,
            })]),
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
    assert!(
        requests[0].body_json()["tools"]
            .as_array()
            .is_some_and(|tools| tools
                .iter()
                .any(|tool| { tool["name"].as_str() == Some("token_miser_search") }))
    );
    let first_request = requests[0].body_json();
    let exec_description = first_request["tools"]
        .as_array()
        .and_then(|tools| {
            tools.iter().find_map(|tool| {
                (tool["name"].as_str() == Some("exec"))
                    .then(|| tool["description"].as_str())
                    .flatten()
            })
        })
        .expect("exec description");
    assert!(exec_description.contains(RESUME_TOOL_GUIDANCE));
    assert!(!exec_description.contains(RESUME_CONTINUATION_GUIDANCE));
    let reduced = output_text(&requests[1].custom_tool_call_output("call-1")["output"]);
    let unreduced = output_text(&requests[3].custom_tool_call_output("call-2")["output"]);
    let ceiling_limited = output_text(&requests[5].custom_tool_call_output("call-3")["output"]);
    assert!(reduced.contains("reducer replacement"));
    assert!(reduced.contains(RESUME_CONTINUATION_GUIDANCE));
    assert!(!reduced.contains(RESUME_TOOL_GUIDANCE));
    assert!(!reduced.contains(NOISE_LINE));
    assert!(unreduced.contains(NOISE_LINE));
    assert!(!unreduced.contains("reducer replacement"));
    assert!(ceiling_limited.contains(NOISE_LINE));
    assert!(!ceiling_limited.contains("reducer replacement"));
    assert!(ceiling_limited.len() < unreduced.len());

    Ok(())
}
