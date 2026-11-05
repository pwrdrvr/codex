use std::fs;
use std::path::Path;
use std::path::PathBuf;

use codex_protocol::ThreadId;
use codex_protocol::protocol::HistoryPosition;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::ThreadHistoryMode;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;

use super::super::LocalThreadStore;
use super::super::test_support::test_config;

fn write_history(
    root: &Path,
    id: ThreadId,
    base: Option<HistoryPosition>,
    messages: &[String],
) -> (PathBuf, Vec<u64>) {
    let folder = root.join("sessions/2026/11/05");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join(format!("rollout-2026-11-05T12-00-00-{id}.jsonl"));
    let start = base.map_or(0, |base| base.end_ordinal_exclusive);
    let meta = RolloutLine {
        timestamp: "2026-11-05T12:00:00Z".to_string(),
        ordinal: Some(start),
        item: RolloutItem::SessionMeta(SessionMetaLine {
            meta: SessionMeta {
                id,
                cwd: root.to_path_buf(),
                timestamp: "2026-11-05T12:00:00Z".to_string(),
                history_mode: ThreadHistoryMode::Paginated,
                history_base: base,
                ..Default::default()
            },
            git: None,
        }),
    };
    let mut bytes = serde_json::to_vec(&meta).unwrap();
    bytes.push(b'\n');
    let mut ends = vec![bytes.len() as u64];
    for (index, text) in messages.iter().enumerate() {
        let value = json!({
            "timestamp": "2026-11-05T12:00:00Z",
            "ordinal": start + index as u64 + 1,
            "type": "response_item",
            "payload": { "type": "message", "role": "user", "content": [{ "type": "input_text", "text": text }] },
        });
        bytes.extend(serde_json::to_vec(&value).unwrap());
        bytes.push(b'\n');
        ends.push(bytes.len() as u64);
    }
    fs::write(&file, bytes).unwrap();
    (file, ends)
}

fn contents(bytes: &[u8]) -> Vec<serde_json::Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .map(|line| {
            let decoded = codex_rollout::parse_rollout_line(line).unwrap();
            assert!(decoded.ordinal.is_none());
            serde_json::to_value(decoded.item).unwrap()
        })
        .collect()
}

// Exhaustive small model: fork history is the frozen parent prefix followed
// by the child's own suffix, independent of physical segment compression.
#[tokio::test]
async fn portable_export_matches_prefix_and_suffix_model_for_every_small_fork_boundary() {
    for parent_len in 0..5 {
        for cutoff in 0..=parent_len {
            for child_len in 0..4 {
                for compressed in [false, true] {
                    let home = TempDir::new().unwrap();
                    let store = LocalThreadStore::new(test_config(home.path()), None);
                    let parent = ThreadId::default();
                    let child = ThreadId::default();
                    let parent_text = (0..parent_len)
                        .map(|i| format!("parent-{i}"))
                        .collect::<Vec<_>>();
                    let child_text = (0..child_len)
                        .map(|i| format!("child-{i}"))
                        .collect::<Vec<_>>();
                    let (parent_path, ends) =
                        write_history(home.path(), parent, None, &parent_text);
                    write_history(
                        home.path(),
                        child,
                        Some(HistoryPosition {
                            thread_id: parent,
                            end_ordinal_exclusive: cutoff as u64 + 1,
                            end_byte_offset: ends[cutoff],
                        }),
                        &child_text,
                    );
                    if compressed {
                        let encoded =
                            zstd::stream::encode_all(fs::read(&parent_path).unwrap().as_slice(), 1)
                                .unwrap();
                        fs::write(parent_path.with_extension("jsonl.zst"), encoded).unwrap();
                        fs::remove_file(parent_path).unwrap();
                    }
                    let bytes = store
                        .export_thread_rollout(child, 1024 * 1024)
                        .await
                        .unwrap();
                    let lines = contents(&bytes);
                    assert_eq!(lines[0]["payload"]["id"], json!(child));
                    assert_eq!(lines[0]["payload"]["history_mode"], "legacy");
                    assert!(lines[0]["payload"].get("history_base").is_none());
                    let actual = lines[1..]
                        .iter()
                        .map(|line| {
                            line["payload"]["content"][0]["text"]
                                .as_str()
                                .unwrap()
                                .to_string()
                        })
                        .collect::<Vec<_>>();
                    let expected = parent_text[..cutoff]
                        .iter()
                        .chain(child_text.iter())
                        .cloned()
                        .collect::<Vec<_>>();
                    assert_eq!(
                        actual, expected,
                        "parent={parent_len} cutoff={cutoff} child={child_len} compressed={compressed}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn portable_export_refuses_a_truncated_or_oversized_snapshot_instead_of_omitting_history() {
    let home = TempDir::new().unwrap();
    let store = LocalThreadStore::new(test_config(home.path()), None);
    let id = ThreadId::default();
    let (file, _) = write_history(home.path(), id, None, &["retained message".to_string()]);
    assert!(store.export_thread_rollout(id, 1).await.is_err());
    use std::io::Write;
    fs::OpenOptions::new()
        .append(true)
        .open(file)
        .unwrap()
        .write_all(b"{broken\n")
        .unwrap();
    assert!(store.export_thread_rollout(id, 1024 * 1024).await.is_err());
}

#[tokio::test]
async fn portable_export_legacy_projection_preserves_paginated_user_messages() {
    use codex_app_server_protocol::ThreadHistoryBuilder;
    use codex_protocol::items::TurnItem;
    use codex_protocol::items::UserMessageItem;
    use codex_protocol::protocol::EventMsg;
    use codex_protocol::protocol::ItemCompletedEvent;
    use codex_protocol::user_input::UserInput;
    use std::io::Write;

    let home = TempDir::new().unwrap();
    let store = LocalThreadStore::new(test_config(home.path()), None);
    let id = ThreadId::default();
    let (file, _) = write_history(home.path(), id, None, &[]);
    let event = RolloutLine {
        timestamp: "2026-11-05T12:00:00Z".to_string(),
        ordinal: Some(1),
        item: RolloutItem::EventMsg(EventMsg::ItemCompleted(ItemCompletedEvent {
            thread_id: id,
            turn_id: "turn-1".to_string(),
            item: TurnItem::UserMessage(UserMessageItem::new(&[UserInput::Text {
                text: "portable question".to_string(),
                text_elements: vec![],
            }])),
            started_at_ms: None,
            completed_at_ms: 0,
        })),
    };
    let mut writer = fs::OpenOptions::new().append(true).open(file).unwrap();
    serde_json::to_writer(&mut writer, &event).unwrap();
    writer.write_all(b"\n").unwrap();
    drop(writer);
    let bytes = store.export_thread_rollout(id, 1024 * 1024).await.unwrap();
    let mut builder = ThreadHistoryBuilder::new();
    for line in std::str::from_utf8(&bytes).unwrap().lines() {
        let line = codex_rollout::parse_rollout_line(line).unwrap();
        if let Some(item) =
            codex_rollout::persisted_rollout_item(&line.item, ThreadHistoryMode::Legacy)
        {
            builder.handle_rollout_item(item.as_ref());
        }
    }
    let turns = builder.finish();
    assert_eq!(
        turns.len(),
        1,
        "legacy fork must retain the visible user turn"
    );
    assert_eq!(turns[0].items.len(), 1);
    assert!(
        serde_json::to_string(&turns[0].items[0])
            .unwrap()
            .contains("portable question")
    );
}
