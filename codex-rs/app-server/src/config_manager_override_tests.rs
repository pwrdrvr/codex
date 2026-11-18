//! Request overrides are a higher-priority layer, not additional process CLI assignments.

use super::*;
use anyhow::Result;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::tempdir;

#[tokio::test]
async fn request_tables_preserve_unrelated_process_settings() -> Result<()> {
    let home = tempdir()?;
    let file_config = "[features]\napps = true\nplugins = true\nmulti_agent = true\ngoals = true\nimage_generation = true\n[mcp_servers.inherited]\ncommand = 'inherited-command'\nenabled = true\n";
    let config_path = home.path().join("config.toml");
    std::fs::write(&config_path, file_config)?;
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    let features = [
        "apps",
        "plugins",
        "multi_agent",
        "goals",
        "image_generation",
    ];

    // Exhaust every combination: an unrelated request table must preserve each
    // process flag's value, irrespective of the file's lower-priority defaults.
    for mask in 0..(1 << features.len()) {
        for dotted_request in [false, true] {
            let mut process = features
                .iter()
                .enumerate()
                .map(|(index, feature)| {
                    (
                        format!("features.{feature}"),
                        TomlValue::Boolean(mask & (1 << index) != 0),
                    )
                })
                .collect::<Vec<_>>();
            process.push(("features.code_mode".into(), TomlValue::Boolean(false)));
            process.push((
                "mcp_servers.inherited.enabled".into(),
                TomlValue::Boolean(false),
            ));
            let code_mode = json!({ "max_output_tokens_ceiling": 1234 });
            let selected = json!({ "command": "selected-command" });
            let request = if dotted_request {
                HashMap::from([
                    ("features.code_mode".into(), code_mode),
                    ("mcp_servers.selected".into(), selected),
                ])
            } else {
                HashMap::from([
                    ("features".into(), json!({ "code_mode": code_mode })),
                    ("mcp_servers".into(), json!({ "selected": selected })),
                ])
            };
            let config = manager
                .load_with_cli_overrides(
                    &process,
                    Some(request),
                    ConfigOverrides::default(),
                    Some(home.path().to_path_buf()),
                )
                .await?;
            let effective = config.config_layer_stack.effective_config();
            for (index, feature) in features.iter().enumerate() {
                assert_eq!(
                    effective["features"][feature],
                    TomlValue::Boolean(mask & (1 << index) != 0),
                    "mask={mask}, dotted_request={dotted_request}, feature={feature}"
                );
            }
            assert_eq!(
                effective["features"]["code_mode"]["enabled"],
                TomlValue::Boolean(false)
            );
            assert_eq!(
                effective["features"]["code_mode"]["max_output_tokens_ceiling"],
                TomlValue::Integer(1234)
            );
            assert!(!config.mcp_servers.get()["inherited"].enabled);
            assert!(config.mcp_servers.get()["selected"].enabled);
        }
    }
    assert_eq!(std::fs::read_to_string(config_path)?, file_config);
    Ok(())
}

#[tokio::test]
async fn explicit_request_values_win_without_splitting_server_names() -> Result<()> {
    let home = tempdir()?;
    let manager = ConfigManager::without_managed_config_for_tests(home.path().to_path_buf());
    for server_name in [
        "inherited",
        "inherited.server",
        "inherited.server.with.dots",
    ] {
        for enabled in [false, true] {
            let process = vec![
                ("features.apps".into(), TomlValue::Boolean(!enabled)),
                (
                    "features.code_mode.max_output_tokens_ceiling".into(),
                    TomlValue::Integer(1234),
                ),
                (
                    "features.code_mode.enabled".into(),
                    TomlValue::Boolean(!enabled),
                ),
                (
                    "mcp_servers".into(),
                    json_to_toml(json!({
                        server_name: {
                            "command": "inherited-command", "enabled": !enabled,
                            "args": ["old"], "env": { "KEPT": "process", "REPLACED": "process" },
                        },
                    })),
                ),
            ];
            let request = HashMap::from([
                (
                    "features".into(),
                    json!({ "apps": enabled, "code_mode": enabled }),
                ),
                (
                    "mcp_servers".into(),
                    json!({
                        server_name: { "enabled": enabled, "args": ["new"], "env": { "REPLACED": "request" } },
                        "selected.server": { "command": "selected-command" },
                    }),
                ),
            ]);
            let config = manager
                .load_with_cli_overrides(
                    &process,
                    Some(request),
                    ConfigOverrides::default(),
                    Some(home.path().to_path_buf()),
                )
                .await?;
            let effective = config.config_layer_stack.effective_config();
            assert_eq!(effective["features"]["apps"], TomlValue::Boolean(enabled));
            assert_eq!(
                effective["features"]["code_mode"]["enabled"],
                TomlValue::Boolean(enabled)
            );
            assert_eq!(
                effective["features"]["code_mode"]["max_output_tokens_ceiling"],
                TomlValue::Integer(1234)
            );
            assert_eq!(
                effective["mcp_servers"],
                json_to_toml(json!({
                    server_name: {
                        "command": "inherited-command", "enabled": enabled,
                        "args": ["new"], "env": { "KEPT": "process", "REPLACED": "request" },
                    },
                    "selected.server": { "command": "selected-command" },
                }))
            );
            assert_eq!(config.mcp_servers.get()[server_name].enabled, enabled);
            assert!(config.mcp_servers.get().contains_key("selected.server"));
        }
    }
    Ok(())
}
