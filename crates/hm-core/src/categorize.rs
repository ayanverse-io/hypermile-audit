//! Token categories and the tool-name → category mapping (spec 01 §3.3).

use serde::Serialize;

/// The categories tokens are attributed to. The string names must match the
/// spec exactly — the audit CLI and desktop app render them verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
pub enum Category {
    /// `Bash` tool output.
    BashOutput,
    /// `Read` tool output.
    FileReads,
    /// `Grep` / `Glob` tool output.
    Search,
    /// `WebFetch` / `WebSearch` tool output.
    Web,
    /// Any `mcp__*` tool output.
    McpTools,
    /// Tool output whose originating tool is unknown or uncategorized.
    OtherTools,
    /// Assistant `text` / `thinking` blocks.
    ModelOutput,
    /// User-authored text.
    YourPrompts,
}

impl Category {
    /// Human-readable name, exactly as it appears in spec 01 §3.3 / §4.
    pub fn name(self) -> &'static str {
        match self {
            Category::BashOutput => "Bash output",
            Category::FileReads => "File reads (Read)",
            Category::Search => "Search (Grep/Glob)",
            Category::Web => "Web (WebFetch/WebSearch)",
            Category::McpTools => "MCP tools",
            Category::OtherTools => "Other tools",
            Category::ModelOutput => "Model output",
            Category::YourPrompts => "Your prompts",
        }
    }

    /// Stable ordering for report display (tool categories first, then the two
    /// authored-content buckets).
    pub const DISPLAY_ORDER: [Category; 8] = [
        Category::BashOutput,
        Category::FileReads,
        Category::Search,
        Category::Web,
        Category::McpTools,
        Category::OtherTools,
        Category::ModelOutput,
        Category::YourPrompts,
    ];
}

/// Map an originating tool name (from the joined `tool_use.name`) to the
/// category its `tool_result` output belongs to.
///
/// `None` — meaning the `tool_use_id` could not be joined to a prior
/// `tool_use` — maps to [`Category::OtherTools`].
pub fn category_for_tool(tool_name: Option<&str>) -> Category {
    let Some(name) = tool_name else {
        return Category::OtherTools;
    };
    // MCP tools are namespaced `mcp__<server>__<tool>`.
    if name.starts_with("mcp__") {
        return Category::McpTools;
    }
    match name {
        "Bash" => Category::BashOutput,
        "Read" => Category::FileReads,
        "Grep" | "Glob" => Category::Search,
        "WebFetch" | "WebSearch" => Category::Web,
        _ => Category::OtherTools,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_tools_map_correctly() {
        assert_eq!(category_for_tool(Some("Bash")), Category::BashOutput);
        assert_eq!(category_for_tool(Some("Read")), Category::FileReads);
        assert_eq!(category_for_tool(Some("Grep")), Category::Search);
        assert_eq!(category_for_tool(Some("Glob")), Category::Search);
        assert_eq!(category_for_tool(Some("WebFetch")), Category::Web);
        assert_eq!(category_for_tool(Some("WebSearch")), Category::Web);
    }

    #[test]
    fn mcp_tools_are_grouped() {
        assert_eq!(
            category_for_tool(Some("mcp__codegraph__codegraph_search")),
            Category::McpTools
        );
    }

    #[test]
    fn unknown_and_missing_map_to_other() {
        assert_eq!(category_for_tool(Some("Edit")), Category::OtherTools);
        assert_eq!(category_for_tool(Some("Write")), Category::OtherTools);
        assert_eq!(category_for_tool(None), Category::OtherTools);
    }

    #[test]
    fn names_match_spec() {
        assert_eq!(Category::BashOutput.name(), "Bash output");
        assert_eq!(Category::FileReads.name(), "File reads (Read)");
        assert_eq!(Category::Search.name(), "Search (Grep/Glob)");
        assert_eq!(Category::Web.name(), "Web (WebFetch/WebSearch)");
        assert_eq!(Category::McpTools.name(), "MCP tools");
        assert_eq!(Category::OtherTools.name(), "Other tools");
        assert_eq!(Category::ModelOutput.name(), "Model output");
        assert_eq!(Category::YourPrompts.name(), "Your prompts");
    }
}
