//! Agent wiki, shared wiki, canvas and identity resolution.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "wiki_ls",
        description: "List wiki pages as a directory tree with titles and last-updated dates. scope=\"shared\" lists the cross-agent wiki. Use to browse; to find pages by content use wiki_search.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_read",
        description: "Read a wiki page (frontmatter + body). Use wiki_ls or wiki_search to find page paths first. Pass scope=\"shared\" to read from the cross-agent shared wiki.",
        params: &[
            ParamDef {
                name: "page_path",
                description: "Page path relative to wiki/ (e.g. 'entities/wang-ming.md')",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_write",
        description: "Create or overwrite a whole wiki page (content incl. YAML frontmatter); atomic, updates _index.md and _log.md. scope=\"shared\" obeys the namespace policy. To edit, wiki_read the page first.",
        params: &[
            ParamDef {
                name: "page_path",
                description: "Page path relative to wiki/ (e.g. 'concepts/return-policy.md')",
                required: true,
            },
            ParamDef {
                name: "content",
                description: "Full page content including YAML frontmatter",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "update_index",
                description: "Add the page to _index.md (default true). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_search",
        description: "Full-text search of wiki pages, trust-weighted, with layer/min_trust filters and optional 1-hop expand. scope=\"shared\" searches the shared wiki. Notes outside the wiki: memory_search.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search query (keywords)",
                required: true,
            },
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max results (default 10, max 100)",
                required: false,
            },
            ParamDef {
                name: "min_trust",
                description: "Drop pages whose trust is below this, 0.0 to 1.0",
                required: false,
            },
            ParamDef {
                name: "layer",
                description: "Filter by layer: identity/core/context/deep",
                required: false,
            },
            ParamDef {
                name: "expand",
                description: "Also return pages one related/backlink hop away (default false). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_lint",
        description: "Audit the wiki for orphan pages, broken links and stale pages; returns a report and changes nothing. scope=\"shared\" audits the shared wiki and its frontmatter schema. Totals only: wiki_stats.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_stats",
        description: "Wiki totals: pages, index entries, recent activity, health score (shared scope adds per-contributor counts). Read-only. To see the problems behind the score use wiki_lint.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent). Ignored when scope=\"shared\".",
                required: false,
            },
            ParamDef {
                name: "scope",
                description: "\"agent\" (your own wiki, the default) or \"shared\" (the cross-agent knowledge base). Shared writes honour the namespace SoT policy — check wiki_namespace_status first.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_export",
        description: "Export a wiki (yours by default) under <home>/exports as one HTML file (default) or an Obsidian vault folder; returns the path. Overwrites the previous export, never the wiki itself.",
        params: &[
            ParamDef {
                name: "format",
                description: "'html' (default, one file) or 'obsidian' (a folder of .md files with wikilinks)",
                required: false,
            },
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_dedup",
        description: "Find likely duplicate wiki pages by title and tag similarity; returns pairs with each page's trust and the reason. Read-only, merges nothing: merge by hand with wiki_write.",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "wiki_graph",
        description: "Render the wiki link graph as a Mermaid diagram (nodes = pages, edges = related links), or only pages within `depth` hops of `center`. Read-only.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "center",
                description: "Center page path for focused view (e.g. 'entities/customer.md')",
                required: false,
            },
            ParamDef {
                name: "depth",
                description: "Max hops from center (default 2, capped at 10; ignored without center)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_rebuild_fts",
        description: "Rebuild the wiki full-text index from the pages on disk; returns the page count. Safe to repeat. Use only when wiki_search misses pages you know exist, e.g. after files were edited directly.",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "wiki_trust_audit",
        description: "List wiki pages whose live trust score fell below a threshold, with citation and signal counters. Use it to spot pages the prediction-error loop is downgrading — they may need checking or archival.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "max_trust",
                description: "Upper bound on trust to include (default: 0.3)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max rows (default: 20, max: 500)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_trust_history",
        description: "Recent audit-history entries for a single wiki page — every trust change with trigger, conversation, signal kind. Use for post-mortem analysis when a page's trust is moving unexpectedly.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent name (default: main agent)",
                required: false,
            },
            ParamDef {
                name: "page_path",
                description: "Page path relative to wiki/ (e.g. 'concepts/cron-facts.md')",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Max rows (default: 50, max: 500)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "shared_wiki_delete",
        description: "Delete a page from the shared wiki. Only the original author or the main agent can delete.",
        params: &[ParamDef {
            name: "page_path",
            description: "Page path to delete",
            required: true,
        }],
    },
    ToolDef {
        name: "wiki_namespace_status",
        description: "Inspect the shared-wiki namespace SoT policy (.scope.toml): each namespace's mode plus its synced_from capability or agent allowlist. Unlisted namespaces are agent_writable. Check before shared writes",
        params: &[],
    },
    ToolDef {
        name: "canvas_push",
        description: "Push an HTML Live Canvas the user views on the dashboard: reports, tables, diagrams. Sanitized server-side and rendered sandboxed, so keep it static and self-contained. Max 256 KB; replaces current.",
        params: &[
            ParamDef {
                name: "html",
                description: "Self-contained static HTML body (max 256 KB; no scripts — they are stripped)",
                required: true,
            },
            ParamDef {
                name: "title",
                description: "Short canvas title shown in the dashboard (optional)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "canvas_clear",
        description: "Clear your Live Canvas on the dashboard (viewers see the empty state; earlier versions stay in history).",
        params: &[],
    },
    ToolDef {
        name: "identity_resolve",
        description: "Resolve a (channel, external_id) pair to the canonical person — name, roles, project memberships, known handles; null when unknown. Use this before judging whether a sender is a project member.",
        params: &[
            ParamDef {
                name: "channel",
                description: "Channel kind: discord / line / telegram / slack / whatsapp / feishu / webchat / email",
                required: true,
            },
            ParamDef {
                name: "external_id",
                description: "Channel-side identifier — Discord user_id, LINE user_id, email address, etc.",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "wiki_share",
        description: "Publish a summary of one of your wiki pages to the shared wiki at sources/<you>--<name>.md with attribution; re-sharing overwrites it. Secret-scanned. Full shared pages: wiki_write scope=shared.",
        params: &[
            ParamDef {
                name: "page_path",
                description: "Page path in your own wiki, relative to wiki/ (e.g. 'concepts/return-policy.md')",
                required: true,
            },
            ParamDef {
                name: "summary",
                description: "Text to publish (default: first 500 characters of the page body)",
                required: false,
            },
        ],
    },
];
