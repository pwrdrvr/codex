use std::collections::BTreeMap;

use codex_protocol::models::ResponseInputItem;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use serde::Deserialize;
use serde_json::Value;

use crate::context::ContextualUserFragment;
use crate::context::TokenMiserRetrievalResult;
use crate::function_tool::FunctionCallError;
use crate::tools::context::FunctionToolOutput;
use crate::tools::context::ToolInvocation;
use crate::tools::context::ToolOutput;
use crate::tools::context::ToolPayload;
use crate::tools::context::boxed_tool_output;
use crate::tools::registry::CoreToolRuntime;
use crate::tools::registry::ToolExecutor;

const READ_TOOL_NAME: &str = "read_token_miser_output";
const SEARCH_TOOL_NAME: &str = "search_token_miser_output";

#[derive(Deserialize)]
struct ReadArgs {
    object_id: String,
    #[serde(default)]
    item_index: usize,
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_read_bytes")]
    max_bytes: usize,
}

#[derive(Deserialize)]
struct SearchArgs {
    object_id: String,
    query: String,
    #[serde(default = "default_search_results")]
    max_results: usize,
}

fn default_read_bytes() -> usize {
    8 * 1024
}

fn default_search_results() -> usize {
    10
}

struct RetrievalOutput(Value);

impl ToolOutput for RetrievalOutput {
    fn log_output(&self) -> String {
        self.0.to_string()
    }

    fn success_for_logging(&self) -> bool {
        true
    }

    fn to_response_item(&self, call_id: &str, payload: &ToolPayload) -> ResponseInputItem {
        let fragment = TokenMiserRetrievalResult::new(self.0.to_string());
        FunctionToolOutput::from_text(fragment.render(), Some(true))
            .to_response_item(call_id, payload)
    }

    fn code_mode_result(&self, _payload: &ToolPayload) -> Value {
        self.0.clone()
    }
}

pub(crate) struct ReadTokenMiserOutputHandler;

impl ToolExecutor<ToolInvocation> for ReadTokenMiserOutputHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(READ_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: READ_TOOL_NAME.to_string(),
            description: concat!(
                "Read one bounded byte range from an exact Token Miser content item. ",
                "Use next_offset to continue only when more of that selected item is needed."
            )
            .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    ("object_id".to_string(), JsonSchema::string(None)),
                    ("item_index".to_string(), JsonSchema::integer(None)),
                    ("offset".to_string(), JsonSchema::integer(None)),
                    ("max_bytes".to_string(), JsonSchema::integer(None)),
                ]),
                Some(vec!["object_id".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Function { arguments } = &invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "read_token_miser_output requires function arguments".to_string(),
                ));
            };
            let args: ReadArgs = serde_json::from_str(arguments).map_err(|err| {
                FunctionCallError::RespondToModel(format!("invalid retrieval arguments: {err}"))
            })?;
            let result = invocation
                .session
                .services
                .code_mode_service
                .read_token_miser_output(
                    invocation.session.thread_id,
                    &args.object_id,
                    args.item_index,
                    args.offset,
                    args.max_bytes,
                )
                .map_err(FunctionCallError::RespondToModel)?;
            invocation
                .session
                .services
                .code_mode_service
                .mark_token_miser_retrieval(&invocation.source);
            Ok(boxed_tool_output(RetrievalOutput(result)))
        })
    }
}

impl CoreToolRuntime for ReadTokenMiserOutputHandler {}

pub(crate) struct SearchTokenMiserOutputHandler;

impl ToolExecutor<ToolInvocation> for SearchTokenMiserOutputHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain(SEARCH_TOOL_NAME)
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: SEARCH_TOOL_NAME.to_string(),
            description: "Search exact retained text and return bounded matching snippets."
                .to_string(),
            strict: false,
            defer_loading: None,
            parameters: JsonSchema::object(
                BTreeMap::from([
                    ("object_id".to_string(), JsonSchema::string(None)),
                    ("query".to_string(), JsonSchema::string(None)),
                    ("max_results".to_string(), JsonSchema::integer(None)),
                ]),
                Some(vec!["object_id".to_string(), "query".to_string()]),
                Some(false.into()),
            ),
            output_schema: None,
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            let ToolPayload::Function { arguments } = &invocation.payload else {
                return Err(FunctionCallError::RespondToModel(
                    "search_token_miser_output requires function arguments".to_string(),
                ));
            };
            let args: SearchArgs = serde_json::from_str(arguments).map_err(|err| {
                FunctionCallError::RespondToModel(format!("invalid search arguments: {err}"))
            })?;
            let result = invocation
                .session
                .services
                .code_mode_service
                .search_token_miser_output(
                    invocation.session.thread_id,
                    &args.object_id,
                    &args.query,
                    args.max_results,
                )
                .map_err(FunctionCallError::RespondToModel)?;
            invocation
                .session
                .services
                .code_mode_service
                .mark_token_miser_retrieval(&invocation.source);
            Ok(boxed_tool_output(RetrievalOutput(result)))
        })
    }
}

impl CoreToolRuntime for SearchTokenMiserOutputHandler {}
