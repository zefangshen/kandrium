//! Versioned instructions for the external authoring agent.
//!
//! The text is compiled into the binary so it always matches the schema this
//! build writes. `get_wiki_instructions` exposes the version and hash; a
//! write tool rejects a stale version rather than letting an agent work from
//! yesterday's rules.

use super::sources::hash_bytes;

/// Current instruction contract version.
pub const WIKI_INSTRUCTIONS_VERSION: u32 = 2;

/// The instruction document shown to agents.
pub const WIKI_INSTRUCTIONS: &str = include_str!("instructions.md");

/// Allowed tag values. Agents are told to use only these.
pub const TAG_VOCABULARY: &[&str] = &[
    "ml",
    "systems",
    "theory",
    "practice",
    "history",
    "people",
    "organizations",
    "method",
    "tooling",
    "reference",
];

/// SHA-256 of the instructions, lowercase hex.
pub fn instructions_hash() -> String {
    hash_bytes(WIKI_INSTRUCTIONS.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn instructions_cover_the_schema_and_tools() {
        for needle in [
            "type",
            "title",
            "summary",
            "created",
            "updated",
            "sources",
            "tags",
            "aliases",
            "reviewed",
            "contradicted_by",
            "get_wiki_instructions",
            "get_wiki_status",
            "list_wiki_sources",
            "read_wiki_source",
            "list_wiki_pages",
            "read_wiki_page",
            "search_wiki",
            "write_wiki_page",
            "patch_wiki_page",
            "delete_wiki_page",
            "begin_wiki_run",
            "heartbeat_wiki_run",
            "end_wiki_run",
            "list_runs",
            "undo_run",
            "record_wiki_run",
            "list_wiki_dedup_candidates",
            "lint_wiki",
            "instructionsVersion",
            "kebab-case",
            "index.md",
            "Merge",
            "Never link a page to itself",
        ] {
            assert!(
                WIKI_INSTRUCTIONS.contains(needle),
                "instructions do not mention {needle}"
            );
        }
        assert!(!TAG_VOCABULARY.is_empty());
        assert_eq!(instructions_hash().len(), 64);
    }
}
