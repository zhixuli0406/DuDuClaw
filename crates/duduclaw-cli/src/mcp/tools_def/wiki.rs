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
        description: "List wiki pages. Returns directory tree with page titles and last-updated timestamps from YAML frontmatter. Pass scope=\"shared\" to list the cross-agent shared wiki instead of your own.",
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
        description: "Create or update a wiki page; _index.md and _log.md update automatically, written atomically. scope=\"shared\" writes the cross-agent wiki, honouring the .scope.toml namespace policy.",
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
                description: "Update _index.md automatically (default: true). Ignored when scope=\"shared\".",
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
        description: "Full-text search across wiki pages with trust-weighted ranking. Supports layer/trust filtering and 1-hop expand via related pages. Pass scope=\"shared\" to search the cross-agent shared wiki.",
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
                description: "Max results (default: 10)",
                required: false,
            },
            ParamDef {
                name: "min_trust",
                description: "Minimum trust score filter (0.0-1.0)",
                required: false,
            },
            ParamDef {
                name: "layer",
                description: "Filter by layer: identity/core/context/deep",
                required: false,
            },
            ParamDef {
                name: "expand",
                description: "1-hop expand via related/backlinks (true/false, default: false). Ignored when scope=\"shared\".",
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
        description: "Health check on the wiki: orphan pages, broken links, stale pages, missing required frontmatter. scope=\"shared\" audits the cross-agent wiki instead of your own.",
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
        description: "Get wiki statistics: total pages, index entries, recent activity, health score. Pass scope=\"shared\" for the cross-agent shared wiki (contributor breakdown) instead of your own.",
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
        description: "Export the wiki as Obsidian vault (directory of .md files with wikilinks) or a single HTML file. Returns the output path or HTML content.",
        params: &[
            ParamDef {
                name: "format",
                description: "Export format: 'obsidian' or 'html' (default: html)",
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
        description: "Detect potential duplicate wiki pages using title and tag similarity. Returns candidate pairs with trust scores for merge decisions.",
        params: &[ParamDef {
            name: "agent_id",
            description: "Agent name (default: main agent)",
            required: false,
        }],
    },
    ToolDef {
        name: "wiki_graph",
        description: "Export wiki knowledge graph as Mermaid diagram. Nodes=pages, edges=related links. Supports focused view around a center page.",
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
                description: "Max hops from center (default: 2, ignored without center)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "wiki_rebuild_fts",
        description: "Rebuild the FTS5 full-text search index from all wiki pages on disk. Use if search results seem stale.",
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
        name: "shared_wiki_ls",
        description: "[deprecated → wiki_ls scope=\"shared\"; removed in v1.68.0] List pages in the shared wiki (~/.duduclaw/shared/wiki/). The shared wiki is a cross-agent public knowledge base.",
        params: &[],
    },
    ToolDef {
        name: "shared_wiki_read",
        description: "[deprecated → wiki_read scope=\"shared\"; removed in v1.68.0] Read a page from the shared wiki. Use shared_wiki_ls or shared_wiki_search to find page paths first.",
        params: &[ParamDef {
            name: "page_path",
            description: "Page path relative to shared/wiki/ (e.g. 'concepts/return-policy.md')",
            required: true,
        }],
    },
    ToolDef {
        name: "shared_wiki_write",
        description: "[deprecated → wiki_write scope=\"shared\"; removed in v1.68.0] Create or update a page in the shared wiki. Author is automatically tracked. All agents can contribute to the shared knowledge base.",
        params: &[
            ParamDef {
                name: "page_path",
                description: "Page path relative to shared/wiki/ (e.g. 'concepts/company-sop.md')",
                required: true,
            },
            ParamDef {
                name: "content",
                description: "Full page content including YAML frontmatter (author field auto-injected)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "shared_wiki_search",
        description: "[deprecated → wiki_search scope=\"shared\"; removed in v1.68.0] Full-text search across shared wiki pages with trust-weighted ranking. Supports layer/trust filtering.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search query (keywords)",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Max results (default: 10)",
                required: false,
            },
            ParamDef {
                name: "min_trust",
                description: "Minimum trust score filter (0.0-1.0)",
                required: false,
            },
            ParamDef {
                name: "layer",
                description: "Filter by layer: identity/core/context/deep",
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
        name: "shared_wiki_stats",
        description: "[deprecated → wiki_stats scope=\"shared\"; removed in v1.68.0] Get shared wiki statistics: total pages, contributor breakdown, recent activity.",
        params: &[],
    },
    ToolDef {
        name: "shared_wiki_lint",
        description: "[deprecated → wiki_lint scope=\"shared\"; removed in v1.68.0] Audit shared wiki pages for schema compliance: missing frontmatter, fallback markers, orphans, broken links, stale pages.",
        params: &[],
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
        description: "Share a page from your wiki to the shared wiki. Creates a source-attributed copy in shared/wiki/sources/.",
        params: &[
            ParamDef {
                name: "page_path",
                description: "Page path in your own wiki to share",
                required: true,
            },
            ParamDef {
                name: "summary",
                description: "Optional custom summary (default: first 500 chars of body)",
                required: false,
            },
        ],
    },
];
