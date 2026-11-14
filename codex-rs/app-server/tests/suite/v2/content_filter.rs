use anyhow::Context;
use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::write_models_cache;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::UserInput;
use core_test_support::responses;
use core_test_support::skip_if_no_network;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::time::Duration;
use tempfile::TempDir;
use test_case::test_case;
use tokio::time::timeout;

fn blocked_response() -> Vec<Value> {
    vec![
        responses::ev_response_created("blocked"),
        json!({
            "type": "response.incomplete",
            "response": { "incomplete_details": { "reason": "content_filter" } }
        }),
    ]
}

fn recovered_response() -> Vec<Value> {
    vec![
        responses::ev_response_created("recovered"),
        responses::ev_assistant_message("alternative", "A permitted alternative."),
        responses::ev_completed("recovered"),
    ]
}

fn prewarm_response() -> Vec<Value> {
    vec![
        responses::ev_response_created("prewarm"),
        responses::ev_completed("prewarm"),
    ]
}

/// Read raw JSON so the test also checks the public spelling and notification count.
async fn run_turn(base_url: &str, websocket: bool) -> Result<(Vec<Value>, Value)> {
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(base_url)
        .with_provider_config(&format!("supports_websockets = {websocket}"))
        .write(codex_home.path())?;
    let config_path = codex_home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?
        .replace("stream_max_retries = 0", "stream_max_retries = 1");
    std::fs::write(config_path, config)?;
    write_models_cache(codex_home.path()).await?;
    let builder = TestAppServer::builder().with_codex_home(codex_home.path());
    // Release builds do not expose the debug-only plugin-startup test flag.
    let builder = if cfg!(debug_assertions) {
        builder
    } else {
        builder.with_plugin_startup_tasks()
    };
    let mut app = builder.build_initialized().await?;
    let thread = app.start_thread(ThreadStartParams::default()).await?.thread;
    let _: TurnStartResponse = app
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "Help me with this topic.".to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        })
        .await?;
    timeout(Duration::from_secs(20), async {
        let mut errors = Vec::new();
        loop {
            let JSONRPCMessage::Notification(notification) = app.read_next_message().await? else {
                continue;
            };
            let params = notification.params.context("notification params")?;
            match notification.method.as_str() {
                "error" => errors.push(params),
                "turn/completed" => return Ok((errors, params)),
                _ => {}
            }
        }
    })
    .await?
}

#[test_case(false; "http")]
#[test_case(true; "websocket")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_filter_recovery_reports_first_retry(websocket: bool) -> Result<()> {
    skip_if_no_network!(Ok(()));
    let http = responses::start_mock_server().await;
    let http_mock = responses::mount_sse_sequence(
        &http,
        vec![
            responses::sse(blocked_response()),
            responses::sse(recovered_response()),
        ],
    )
    .await;
    let ws = responses::start_websocket_server(vec![
        vec![prewarm_response(), blocked_response()],
        vec![recovered_response()],
    ])
    .await;
    let http_uri = http.uri();
    let base_url = if websocket { ws.uri() } else { &http_uri };
    let (errors, completed) = run_turn(base_url, websocket).await?;
    assert_eq!(errors.len(), 1, "{errors:#?}");
    assert_eq!(errors[0]["willRetry"], true);
    assert_eq!(errors[0]["error"]["codexErrorInfo"], "contentFilter");
    assert_eq!(
        errors[0]["error"]["message"],
        "Response blocked by content filter; retrying (1/1)"
    );
    assert!(
        errors[0]["error"]["additionalDetails"]
            .as_str()
            .context("retry details")?
            .contains("reason: content_filter")
    );
    assert_eq!(completed["turn"]["status"], "completed");
    assert_eq!(completed["turn"]["error"], Value::Null);
    if websocket {
        assert_eq!(ws.connections().iter().map(Vec::len).sum::<usize>(), 3);
    } else {
        assert_eq!(http_mock.requests().len(), 2);
    }
    ws.shutdown().await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn content_filter_exhaustion_reports_failed_turn() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let server = responses::start_mock_server().await;
    let mock =
        responses::mount_sse_sequence(&server, vec![responses::sse(blocked_response()); 2]).await;
    let (errors, completed) = run_turn(&server.uri(), false).await?;
    assert_eq!(errors.len(), 2, "{errors:#?}");
    assert_eq!(errors[0]["willRetry"], true);
    assert_eq!(errors[1]["willRetry"], false);
    for error in errors {
        assert_eq!(error["error"]["codexErrorInfo"], "contentFilter");
    }
    assert_eq!(completed["turn"]["status"], "failed");
    assert_eq!(
        completed["turn"]["error"]["codexErrorInfo"],
        "contentFilter"
    );
    assert_eq!(mock.requests().len(), 2);
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ordinary_first_websocket_reconnect_stays_hidden() -> Result<()> {
    skip_if_no_network!(Ok(()));
    let ws = responses::start_websocket_server(vec![
        vec![
            prewarm_response(),
            vec![responses::ev_response_created("disconnected")],
        ],
        vec![recovered_response()],
    ])
    .await;
    let (errors, completed) = run_turn(ws.uri(), true).await?;
    assert_eq!(errors.len(), usize::from(cfg!(debug_assertions)));
    assert_eq!(completed["turn"]["status"], "completed");
    assert_eq!(ws.connections().iter().map(Vec::len).sum::<usize>(), 3);
    ws.shutdown().await;
    Ok(())
}
