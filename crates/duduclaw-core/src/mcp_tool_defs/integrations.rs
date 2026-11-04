//! Notion, GitHub, cost telemetry and media transcode tools.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "notion_status",
        description: "Check the Notion connection: connected/not. Reads local state only — no Notion API call. Call this first if a Notion tool reports an auth error.",
        params: &[],
    },
    ToolDef {
        name: "notion_search",
        description: "Search pages and databases shared with your Notion integration (read-only). Returns id/title/type/last-edited/url for each match. Notion content is an external reference source, not the shared wiki.",
        params: &[
            ParamDef {
                name: "query",
                description: "Search text (matches page/database titles)",
                required: true,
            },
            ParamDef {
                name: "max_results",
                description: "Max results to return (1-25, default 10)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "notion_page_read",
        description: "Read one Notion page in full (read-only): title, url, last-edited, and the page body flattened to plain text (common block types; up to ~200 blocks). Use notion_search to find a page_id first.",
        params: &[ParamDef {
            name: "page_id",
            description: "Notion page ID (from notion_search results)",
            required: true,
        }],
    },
    ToolDef {
        name: "notion_page_append",
        description: "Append text as new paragraph blocks to an existing Notion page (write). Each non-empty line becomes a paragraph; existing content is never deleted or overwritten.",
        params: &[
            ParamDef {
                name: "page_id",
                description: "Notion page ID to append to",
                required: true,
            },
            ParamDef {
                name: "text",
                description: "Text to append (one paragraph block per non-empty line)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "github_status",
        description: "Check the GitHub connection: connected/not, granted scopes. Reads local state only — no GitHub API call. Call this first if a GitHub tool reports an auth error.",
        params: &[],
    },
    ToolDef {
        name: "github_search_issues",
        description: "Search GitHub issues and pull requests (read-only). Uses GitHub search syntax, e.g. 'repo:owner/name is:open label:bug' or 'author:alice is:pr'. Returns repo/number/title/state/is_pr/updated/url.",
        params: &[
            ParamDef {
                name: "query",
                description: "GitHub issues search query",
                required: true,
            },
            ParamDef {
                name: "max_results",
                description: "Max results to return (1-25, default 10)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "github_issue_read",
        description: "Read one GitHub issue in full (read-only): title, state, author, body (truncated if long), and the most recent 10 comments.",
        params: &[
            ParamDef {
                name: "owner",
                description: "Repository owner (user or org)",
                required: true,
            },
            ParamDef {
                name: "repo",
                description: "Repository name",
                required: true,
            },
            ParamDef {
                name: "number",
                description: "Issue number",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "github_pr_read",
        description: "Read one GitHub pull request (read-only): metadata (base/head/state/merged/mergeable) plus the changed-file list (filename/status/additions/deletions, up to 50 files). Diff contents are not fetched.",
        params: &[
            ParamDef {
                name: "owner",
                description: "Repository owner (user or org)",
                required: true,
            },
            ParamDef {
                name: "repo",
                description: "Repository name",
                required: true,
            },
            ParamDef {
                name: "number",
                description: "Pull request number",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "github_issue_comment",
        description: "Post a comment on a GitHub issue or PR (write). This is a PUBLICLY VISIBLE external statement — treat it as outbound communication.",
        params: &[
            ParamDef {
                name: "owner",
                description: "Repository owner (user or org)",
                required: true,
            },
            ParamDef {
                name: "repo",
                description: "Repository name",
                required: true,
            },
            ParamDef {
                name: "number",
                description: "Issue or PR number",
                required: true,
            },
            ParamDef {
                name: "body",
                description: "Comment body (Markdown supported)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "cost_summary",
        description: "Token usage and cost summary (global or per-agent): cache efficiency, total tokens, estimated cost, plus a `by_model` breakdown of the same window, costliest first.",
        params: &[
            ParamDef {
                name: "agent_id",
                description: "Agent ID to filter (optional, omit for global summary)",
                required: false,
            },
            ParamDef {
                name: "hours",
                description: "Time window in hours (default 24)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "cost_agents",
        description: "List all agents ranked by cost: per-agent cache efficiency and health status under `agents`, plus a `by_model` breakdown of the same window, costliest first.",
        params: &[ParamDef {
            name: "hours",
            description: "Time window in hours (default 24)",
            required: false,
        }],
    },
    ToolDef {
        name: "cost_users",
        description: "List end users ranked by cost over a time window (WP6 — 'which employee is spending?'). Unattributed/system traffic is bucketed under '(system)'. Admin scope.",
        params: &[ParamDef {
            name: "hours",
            description: "Time window in hours (default 24)",
            required: false,
        }],
    },
    ToolDef {
        name: "cost_recent",
        description: "Show recent individual API call records with detailed token breakdown (input, cache_read, cache_write, output).",
        params: &[ParamDef {
            name: "limit",
            description: "Number of recent records (default 20)",
            required: false,
        }],
    },
    ToolDef {
        name: "cost_multi_vs_single",
        description: "Honest multi-agent vs single-agent cost report: delegated-work cost ('dispatch') vs direct-reply cost ('chat') per agent per day, with window totals. Per-episode linkage is not derivable and says so.",
        params: &[ParamDef {
            name: "days",
            description: "Time window in days (default 7, max 365)",
            required: false,
        }],
    },
    ToolDef {
        name: "transcribe_audio",
        description: "Transcribe audio to text using Whisper ASR. Accepts base64-encoded audio (OGG/MP3/WAV/M4A). Returns transcribed text. Default language: zh (Mandarin).",
        params: &[
            ParamDef {
                name: "audio_base64",
                description: "Base64-encoded audio data",
                required: true,
            },
            ParamDef {
                name: "language",
                description: "Language hint (default: zh). BCP-47 code.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "meeting_transcribe",
        description: "將自己的附件音訊或影片交由設定的服務商轉錄，並在背景啟動工作。",
        params: &[
            ParamDef { name: "source_path", description: "自己附件目錄內的錄音或影片路徑。", required: false },
            ParamDef { name: "drive_file_id", description: "目前不支援影音下載；請先下載並上傳至員工附件。不可與 source_path 同時使用。", required: false },
            ParamDef { name: "engine", description: "gemini、google_stt 或 whisper；員工只能使用管理員設定的引擎。", required: false },
            ParamDef { name: "language_hint", description: "錄音語言提示，預設 zh-TW。輸出文字會轉成臺灣繁體。", required: false },
            ParamDef { name: "write_wiki", description: "目前僅支援 false；請檢視附件後再透過知識庫工具存入。", required: false },
        ],
    },
    ToolDef {
        name: "meeting_upload_link",
        description: "建立給目前對話使用者的單次會議錄音上傳連結，30 分鐘內有效；上傳完成後在原對話通知。需要管理員設定控制台公開網址。",
        params: &[],
    },
    ToolDef {
        name: "meeting_transcribe_status",
        description: "查詢自己的會議轉錄進度、費用與逐字稿附件。",
        params: &[ParamDef {name:"job_id",description:"會議轉錄工作編號。",required:true}],
    },
    ToolDef {
        name: "meeting_transcribe_cancel",
        description: "取消自己的會議轉錄；目前已送出的音訊段會先完成並記錄費用，之後不再開始新的段落。",
        params: &[ParamDef {name:"job_id",description:"會議轉錄工作編號。",required:true}],
    },
    ToolDef {
        name: "synthesize_speech",
        description: "Convert text to speech with installed local Kokoro or Piper models. Returns base64 MP3 or WAV audio. Gateway-held paid TTS keys are unavailable here.",
        params: &[
            ParamDef {
                name: "text",
                description: "Text to synthesize",
                required: true,
            },
            ParamDef {
                name: "voice",
                description: "Provider-specific voice name (default: configured voice, then provider auto-detection)",
                required: false,
            },
        ],
    },
];
