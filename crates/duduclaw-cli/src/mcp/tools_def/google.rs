//! Google Workspace (Gmail / Calendar / Sheets / Forms / Tasks / Drive / Docs / Slides).
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "google_status",
        description: "Check the Google Workspace connection: connected or not, granted scopes, token validity and expiry. Reads local state only. Call this first if a Gmail/Calendar tool reports an auth error.",
        params: &[],
    },
    ToolDef {
        name: "gmail_search",
        description: "Search the connected Gmail mailbox (read-only). Uses Gmail search syntax, e.g. 'from:alice is:unread' or 'subject:invoice newer_than:7d'. Returns sender/subject/date/snippet for each match.",
        params: &[
            ParamDef {
                name: "query",
                description: "Gmail search query (Gmail search operators supported)",
                required: true,
            },
            ParamDef {
                name: "max_results",
                description: "Max messages to return (1-25, default 10)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "gmail_read",
        description: "Read one Gmail message in full (read-only): headers, plain-text body (long bodies truncated), and an attachment list (filename + size only — attachments are not downloaded).",
        params: &[ParamDef {
            name: "message_id",
            description: "Gmail message ID (from gmail_search results)",
            required: true,
        }],
    },
    ToolDef {
        name: "gmail_create_draft",
        description: "Create a Gmail DRAFT for human review. This only saves a draft and NEVER sends — sending stays a manual human action.",
        params: &[
            ParamDef {
                name: "to",
                description: "Recipient email address",
                required: true,
            },
            ParamDef {
                name: "subject",
                description: "Email subject",
                required: true,
            },
            ParamDef {
                name: "body",
                description: "Plain-text email body",
                required: true,
            },
            ParamDef {
                name: "cc",
                description: "CC email address (optional)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "calendar_list_events",
        description: "List events on the connected primary Google Calendar (read-only). Defaults to the next 7 days when no time range is given. Returns summary/start/end/location plus any Google Meet link.",
        params: &[
            ParamDef {
                name: "time_min",
                description: "Range start, RFC-3339 (default: now)",
                required: false,
            },
            ParamDef {
                name: "time_max",
                description: "Range end, RFC-3339 (default: now + 7 days)",
                required: false,
            },
            ParamDef {
                name: "max_results",
                description: "Max events to return (1-50, default 20)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "calendar_create_event",
        description: "Create a REAL event on the connected primary Google Calendar — externally visible, attendees are notified. Set with_meet=true to attach a Google Meet link.",
        params: &[
            ParamDef {
                name: "summary",
                description: "Event title",
                required: true,
            },
            ParamDef {
                name: "start",
                description: "Start time, RFC-3339 (e.g. 2026-07-26T14:00:00+08:00)",
                required: true,
            },
            ParamDef {
                name: "end",
                description: "End time, RFC-3339",
                required: true,
            },
            ParamDef {
                name: "description",
                description: "Event description (optional)",
                required: false,
            },
            ParamDef {
                name: "attendees",
                description: "Comma-separated attendee emails (optional)",
                required: false,
            },
            ParamDef {
                name: "with_meet",
                description: "Attach a Google Meet link (true/false, default false)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "sheets_read",
        description: "Read a range of cells from a connected Google Sheet (read-only). Accepts a spreadsheet ID or a full spreadsheet URL. Returns up to 200 rows of formatted cell values.",
        params: &[
            ParamDef {
                name: "spreadsheet_id",
                description: "Spreadsheet ID or full Google Sheets URL",
                required: true,
            },
            ParamDef {
                name: "range",
                description: "A1 range, optionally sheet-qualified (e.g. 'Sheet1!A1:C10' or 'A:D')",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "sheets_append",
        description: "Append one row of values to a connected Google Sheet (write). Values are entered as if typed (USER_ENTERED), so numbers, dates and formulas are parsed.",
        params: &[
            ParamDef {
                name: "spreadsheet_id",
                description: "Spreadsheet ID or full Google Sheets URL",
                required: true,
            },
            ParamDef {
                name: "range",
                description: "A1 range identifying the table to append to (e.g. 'Sheet1!A1')",
                required: true,
            },
            ParamDef {
                name: "values",
                description: "Row cells: JSON array of strings, or a comma-separated list",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "forms_get",
        description: "Read a Google Form's structure (read-only): title, description, and every question with its question_id, type and choice options. Call this before forms_list_responses.",
        params: &[ParamDef {
            name: "form_id",
            description: "Form ID or full Google Forms URL (editor or viewform link)",
            required: true,
        }],
    },
    ToolDef {
        name: "forms_list_responses",
        description: "List a Google Form's submitted responses (read-only, newest page first, up to 50): responseId, submitted time, respondent email when collected, answers keyed by question_id — map ids with forms_get.",
        params: &[ParamDef {
            name: "form_id",
            description: "Form ID or full Google Forms URL",
            required: true,
        }],
    },
    ToolDef {
        name: "gtasks_lists",
        description: "List the connected Google Tasks account's task lists (read-only). Returns each list's id + title; pass an id as task_list_id to the other gtasks_* tools ('@default' targets the user's default list).",
        params: &[],
    },
    ToolDef {
        name: "gtasks_list",
        description: "List tasks in one Google Tasks list (read-only). Pending tasks only by default; set show_completed=true to include finished ones. Returns id/title/notes/status/due for each.",
        params: &[
            ParamDef {
                name: "task_list_id",
                description: "Task list ID from gtasks_lists, or '@default'",
                required: false,
            },
            ParamDef {
                name: "show_completed",
                description: "Include completed tasks (true/false, default false)",
                required: false,
            },
            ParamDef {
                name: "max_results",
                description: "Max tasks to return (1-100, default 50)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "gtasks_create",
        description: "Create a task in a Google Tasks list (write — appears in the user's real Google Tasks). Operators may gate this via agent.toml [capabilities] approval_required_tools.",
        params: &[
            ParamDef {
                name: "title",
                description: "Task title",
                required: true,
            },
            ParamDef {
                name: "task_list_id",
                description: "Task list ID from gtasks_lists, or '@default'",
                required: false,
            },
            ParamDef {
                name: "notes",
                description: "Task notes/details (optional)",
                required: false,
            },
            ParamDef {
                name: "due",
                description: "Due date, RFC-3339 (e.g. 2026-08-01T00:00:00Z). Google Tasks keeps the date part only.",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "gtasks_complete",
        description: "Mark a Google Tasks task as completed (write). Get task ids from gtasks_list.",
        params: &[
            ParamDef {
                name: "task_id",
                description: "Task ID from gtasks_list",
                required: true,
            },
            ParamDef {
                name: "task_list_id",
                description: "Task list ID from gtasks_lists, or '@default'",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "drive_search",
        description: "Search the connected Google Drive (read-only): matches file names and full text, skips trashed, newest first. Feed a returned id to drive_read, docs_read, sheets_read or slides_read.",
        params: &[
            ParamDef {
                name: "query",
                description: "Free-text search term (matched against file names and contents)",
                required: true,
            },
            ParamDef {
                name: "mime_type",
                description: "Optional exact MIME filter, e.g. application/vnd.google-apps.document, .spreadsheet, .presentation, or application/pdf.",
                required: false,
            },
            ParamDef {
                name: "max_results",
                description: "Max files to return (1-50, default 20)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "drive_read",
        description: "Read a Drive file as text (read-only). Docs/Slides export as plain text, Sheets as CSV (first sheet only — use sheets_read for a tab). Binary types return metadata plus a note, never bytes.",
        params: &[ParamDef {
            name: "file_id",
            description: "Drive file ID or any Google share URL",
            required: true,
        }],
    },
    ToolDef {
        name: "docs_read",
        description: "Read a Google Doc's text (read-only): title plus body text in document order, including table cell text. Long documents are truncated.",
        params: &[ParamDef {
            name: "document_id",
            description: "Document ID or full Google Docs URL",
            required: true,
        }],
    },
    ToolDef {
        name: "docs_append",
        description: "Append text to the END of a Google Doc (write). Append-only by design — no tool rewrites or deletes existing content. Newlines start new paragraphs.",
        params: &[
            ParamDef {
                name: "document_id",
                description: "Document ID or full Google Docs URL",
                required: true,
            },
            ParamDef {
                name: "text",
                description: "Text to append (newlines create paragraphs)",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "slides_read",
        description: "Read a Google Slides deck's text slide by slide (read-only): title plus each slide's shape, grouped-shape and table text. There is no Slides write tool — generate .pptx with the office tools.",
        params: &[ParamDef {
            name: "presentation_id",
            description: "Presentation ID or full Google Slides URL",
            required: true,
        }],
    },
];
