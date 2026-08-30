use std::sync::LazyLock;

use rmcp::model::Tool;
use serde::Deserialize;

const PINNED_CATALOG_JSON: &str = include_str!("../schemas/tavily-mcp-0.2.22.json");

#[derive(Deserialize)]
struct PinnedCatalog {
    tools: Vec<Tool>,
}

static CANONICAL_TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    serde_json::from_str::<PinnedCatalog>(PINNED_CATALOG_JSON)
        .expect("embedded Tavily MCP catalog must be valid")
        .tools
});

pub fn canonical_tools() -> &'static [Tool] {
    &CANONICAL_TOOLS
}

pub fn canonical_tool(name: impl AsRef<str>) -> Option<&'static Tool> {
    let name = name.as_ref();
    CANONICAL_TOOLS.iter().find(|tool| tool.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pinned_catalog_contains_every_official_tool() {
        assert_eq!(
            canonical_tools()
                .iter()
                .map(|tool| tool.name.as_ref())
                .collect::<Vec<_>>(),
            [
                "tavily_search",
                "tavily_extract",
                "tavily_crawl",
                "tavily_map",
                "tavily_research",
            ]
        );
        for tool in canonical_tools() {
            assert_eq!(tool.input_schema.get("type"), Some(&"object".into()));
            assert!(tool.input_schema.get("required").is_some(), "{}", tool.name);
        }
    }

    #[test]
    fn pinned_catalog_records_its_source_version() {
        let catalog: serde_json::Value = serde_json::from_str(PINNED_CATALOG_JSON).unwrap();
        assert_eq!(catalog["packageVersion"], "0.2.22");
        assert_eq!(
            catalog["commit"],
            "248dc9e3e385305ad3281120284ff662af4b5940"
        );
    }
}
