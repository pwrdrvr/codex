use codex_code_mode::ImageDetailVisibility;
use codex_code_mode::ToolDefinition as CodeModeToolDefinition;
use codex_tools::FreeformTool;
use codex_tools::FreeformToolFormat;
use codex_tools::ToolSpec;
use std::collections::BTreeMap;

const OUTPUT_REDUCTION_GUIDANCE: &str = r#"Output reduction is applied only after a cell finishes. Nested tool return values remain complete inside the JavaScript cell.
When several nested operations are independent, continue to run them concurrently with `Promise.all`. Inspect or transform their results inside JavaScript and emit one compact combined result. Do not serialize independent operations or narrow them one at a time merely to avoid output reduction."#;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum OutputReductionGuidance {
    Omit,
    Include,
}

pub(crate) fn create_code_mode_tool(
    enabled_tools: &[CodeModeToolDefinition],
    deferred_tools: &[CodeModeToolDefinition],
    namespace_descriptions: &BTreeMap<String, codex_code_mode::ToolNamespaceDescription>,
    default_exec_yield_time_ms: u64,
    code_mode_only: bool,
    image_detail_visibility: ImageDetailVisibility,
    output_reduction_guidance: OutputReductionGuidance,
) -> ToolSpec {
    const CODE_MODE_FREEFORM_GRAMMAR: &str = r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#;

    let mut description = codex_code_mode::build_exec_tool_description(
        enabled_tools,
        deferred_tools,
        namespace_descriptions,
        default_exec_yield_time_ms,
        code_mode_only,
        image_detail_visibility,
    );
    if output_reduction_guidance == OutputReductionGuidance::Include {
        description.push_str("\n\n");
        description.push_str(OUTPUT_REDUCTION_GUIDANCE);
    }

    ToolSpec::Freeform(FreeformTool {
        name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
        description,
        defer_loading: None,
        format: FreeformToolFormat {
            r#type: "grammar".to_string(),
            syntax: "lark".to_string(),
            definition: CODE_MODE_FREEFORM_GRAMMAR.to_string(),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_tools::ToolName;
    use pretty_assertions::assert_eq;

    #[test]
    fn create_code_mode_tool_matches_expected_spec() {
        let enabled_tools = vec![codex_code_mode::ToolDefinition {
            name: "update_plan".to_string(),
            tool_name: ToolName::plain("update_plan"),
            description: "Update the plan".to_string(),
            kind: codex_code_mode::CodeModeToolKind::Function,
            input_schema: None,
            output_schema: None,
        }];

        assert_eq!(
            create_code_mode_tool(
                &enabled_tools,
                &[],
                &BTreeMap::new(),
                codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
                /*code_mode_only*/ true,
                ImageDetailVisibility::Visible,
                OutputReductionGuidance::Omit,
            ),
            ToolSpec::Freeform(FreeformTool {
                name: codex_code_mode::PUBLIC_TOOL_NAME.to_string(),
                description: codex_code_mode::build_exec_tool_description(
                    &enabled_tools,
                    &[],
                    &BTreeMap::new(),
                    codex_code_mode::DEFAULT_EXEC_YIELD_TIME_MS,
                    /*code_mode_only*/ true,
                    ImageDetailVisibility::Visible,
                ),
                defer_loading: None,
                format: FreeformToolFormat {
                    r#type: "grammar".to_string(),
                    syntax: "lark".to_string(),
                    definition: r#"
start: pragma_source | plain_source
pragma_source: PRAGMA_LINE NEWLINE SOURCE
plain_source: SOURCE

PRAGMA_LINE: /[ \t]*\/\/ @exec:[^\r\n]*/
NEWLINE: /\r?\n/
SOURCE: /[\s\S]+/
"#
                    .to_string(),
                },
            })
        );
    }
}
