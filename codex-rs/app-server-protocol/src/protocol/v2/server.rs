use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ServerCapabilitiesReadParams {}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ServerCapabilitiesReadResponse {
    pub code_mode_output_reducer: CodeModeOutputReducerCapability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOutputReducerCapability {
    pub protocol_version: u32,
    pub config_key: String,
    pub max_output_tokens_ceiling_config_key: String,
    pub post_tool_use_nested_context_field: String,
    pub supports_thread_resume_overrides: bool,
    pub acceptance: CodeModeOutputReducerAcceptanceCapability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOutputReducerAcceptanceCapability {
    pub descriptor_url_field: String,
    pub response_id_field: String,
    pub callback_version: u32,
}
