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
    pub pwrdrvr_token_miser: PwrdrvrTokenMiserCapability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PwrdrvrTokenMiserCapability {
    pub version: u32,
    pub identity: String,
    pub initialize_capability_field: String,
    pub thread_start_field: String,
    pub thread_resume_field: String,
    pub descriptor_environment_variable: String,
    pub descriptor_version: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOutputReducerCapability {
    pub protocol_version: u32,
    pub continuation_guidance_version: u32,
    pub intent_context_version: u32,
    pub reducer_request_field: String,
    pub post_tool_use_field: String,
    pub model_guidance: CodeModeOutputReducerModelGuidanceCapability,
    pub actionable_state: CodeModeActionableStateCapability,
    pub config_key: String,
    pub max_output_tokens_ceiling_config_key: String,
    pub post_tool_use_nested_context_field: String,
    pub post_tool_use_grouping: CodeModePostToolUseGroupingCapability,
    pub post_tool_use_exact_output: CodeModePostToolUseExactOutputCapability,
    pub supports_thread_resume_overrides: bool,
    pub dynamic_tools_resume_field: String,
    pub acceptance: CodeModeOutputReducerAcceptanceCapability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOutputReducerModelGuidanceCapability {
    pub version: u32,
    pub tool_description_config_key: String,
    pub continuation_config_key: String,
    pub model_visible_overhead_request_field: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeActionableStateCapability {
    pub version: u32,
    pub reducer_request_field: String,
    pub reducer_response_field: String,
    pub model_output_tag: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModePostToolUseGroupingCapability {
    pub version_field: String,
    pub version: u32,
    pub cell_id_field: String,
    pub tool_call_id_field: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModePostToolUseExactOutputCapability {
    pub version: u32,
    pub version_field: String,
    pub response_field: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct CodeModeOutputReducerAcceptanceCapability {
    pub descriptor_url_field: String,
    pub response_id_field: String,
    pub callback_version: u32,
    pub direct_post_tool_use: DirectPostToolUseAcceptanceCapability,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct DirectPostToolUseAcceptanceCapability {
    pub hook_response_id_field: String,
    pub hook_acceptance_version_field: String,
    pub hook_acceptance_version: u32,
    pub session_id_field: String,
    pub turn_id_field: String,
    pub tool_use_id_field: String,
}
