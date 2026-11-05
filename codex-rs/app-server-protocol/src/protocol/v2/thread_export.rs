use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Export a complete portable snapshot, without requiring clients to inspect storage.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadExportParams {
    pub thread_id: String,
    /// Maximum decoded snapshot size; defaults to 64 MiB and cannot exceed 128 MiB.
    #[ts(optional)]
    pub max_bytes: Option<u32>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadExportResponse {
    pub thread_id: String,
    /// Versioned, self-contained JSONL accepted by thread/fork.path.
    pub format: String,
    pub data_base64: String,
}
