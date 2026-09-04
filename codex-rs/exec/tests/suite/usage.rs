use core_test_support::responses;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex_exec::test_codex_exec;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use walkdir::WalkDir;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn harbor_shaped_exec_json_and_rollout_preserve_authoritative_usage() -> anyhow::Result<()> {
    skip_if_no_network!(Ok(()));

    let test = test_codex_exec();
    let server = responses::start_mock_server().await;
    let response = responses::sse(vec![
        responses::ev_response_created("usage-response"),
        responses::ev_assistant_message("usage-message", "done"),
        json!({
            "type": "response.completed",
            "response": {
                "id": "usage-response",
                "usage": {
                    "input_tokens": 10,
                    "input_tokens_details": {
                        "cached_tokens": 3,
                        "cache_write_tokens": 4
                    },
                    "output_tokens": 29,
                    "output_tokens_details": { "reasoning_tokens": 7 },
                    "total_tokens": 42
                }
            }
        }),
    ]);
    let _response_mock = responses::mount_sse_once(&server, response).await;

    let output = test
        .cmd_with_server(&server)
        .arg("--skip-git-repo-check")
        .arg("--json")
        .arg("report usage")
        .output()?;
    assert!(output.status.success(), "exec run failed: {output:?}");

    let events = String::from_utf8(output.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let completed = events
        .iter()
        .filter(|event| event["type"] == "turn.completed")
        .collect::<Vec<_>>();
    assert_eq!(
        completed,
        vec![&json!({
            "type": "turn.completed",
            "usage": {
                "total_tokens": 42,
                "input_tokens": 10,
                "cached_input_tokens": 3,
                "cache_write_input_tokens": 4,
                "output_tokens": 29,
                "reasoning_output_tokens": 7
            }
        })]
    );

    let rollout_path = WalkDir::new(test.home_path().join("sessions"))
        .into_iter()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.file_type().is_file() && entry.file_name().to_string_lossy().ends_with(".jsonl")
        })
        .expect("codex exec should persist one rollout")
        .into_path();
    let rollout_items = std::fs::read_to_string(rollout_path)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let total = rollout_items.iter().rev().find_map(|item| {
        (item["type"] == "event_msg" && item["payload"]["type"] == "token_count")
            .then(|| item["payload"]["info"]["total_token_usage"].clone())
            .filter(|usage| !usage.is_null())
    });
    assert_eq!(
        total,
        Some(json!({
            "input_tokens": 10,
            "cached_input_tokens": 3,
            "cache_write_input_tokens": 4,
            "output_tokens": 29,
            "reasoning_output_tokens": 7,
            "total_tokens": 42
        }))
    );

    Ok(())
}
