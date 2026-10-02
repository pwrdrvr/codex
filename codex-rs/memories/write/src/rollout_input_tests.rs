use super::*;
use pretty_assertions::assert_eq;

#[test]
fn token_miser_internal_records_are_excluded_from_memory_evidence() -> anyhow::Result<()> {
    let identity = serde_json::json!({
        "version": 1,
        "object_id": "00000000-0000-4000-8000-000000000001",
        "thread_id": "00000000-0000-0000-0000-000000000042",
        "turn_id": "turn-1",
        "call_id": "call-1",
        "cell_id": "cell-1"
    });
    let mut raw = identity.clone();
    raw["script_status"] = serde_json::json!("Script completed");
    raw["success"] = serde_json::json!(true);
    raw["content_items"] = serde_json::json!([
        {"type": "input_text", "text": "private raw tool output"}
    ]);
    let mut decision = identity;
    decision["outcome"] = serde_json::json!({
        "decision": "replace", "replacement": "internal decision payload"
    });
    decision["usage"] = serde_json::Value::Null;
    let records = [
        serde_json::from_value(serde_json::json!({"type": "token_miser_output", "payload": raw}))?,
        serde_json::from_value(
            serde_json::json!({"type": "token_miser_decision", "payload": decision}),
        )?,
    ];
    assert_eq!(
        serialize_tiered_input(&records, /*token_limit*/ 10_000)?,
        serialize_tiered_input(&[], /*token_limit*/ 10_000)?,
    );
    Ok(())
}

#[test]
fn extraction_chunks_preserve_unicode_evidence_with_bounded_messages() {
    let evidence = "User correction: 🐈\n".repeat(2_000);
    let mut reconstructed = String::new();
    for message in extraction_messages(&evidence) {
        let ResponseItem::Message { role, content, .. } = message else {
            panic!("message")
        };
        assert_eq!(role, "user");
        let [ContentItem::InputText { text }] = content.as_slice() else {
            panic!("text")
        };
        assert!(text.len() < 9_000);
        reconstructed.push_str(text);
    }
    assert_eq!(reconstructed, evidence);
}

#[test]
fn classifies_memory_excluded_fragments() {
    let cases = [
        (
            "# AGENTS.md instructions for /tmp\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>",
            true,
        ),
        (
            "# AGENTS.md instructions\n\n<INSTRUCTIONS>\nbody\n</INSTRUCTIONS>",
            true,
        ),
        (
            "<skill>\n<name>demo</name>\n<path>skills/demo/SKILL.md</path>\nbody\n</skill>",
            true,
        ),
        (
            "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
            false,
        ),
        (
            "<subagent_notification>{\"agent_id\":\"a\",\"status\":\"completed\"}</subagent_notification>",
            false,
        ),
    ];

    for (text, expected) in cases {
        assert_eq!(
            is_memory_excluded_contextual_user_fragment(&ContentItem::InputText {
                text: text.to_string(),
            }),
            expected,
            "{text}",
        );
    }
}
