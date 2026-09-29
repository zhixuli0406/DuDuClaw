//! Read-only SQL connector and local data-file readers.
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "db_sources",
        description: "List the read-only SQL data sources this agent may query (name, label, driver). Call this first to discover what is available. Requires the agent's [capabilities] db_sources grant.",
        params: &[],
    },
    ToolDef {
        name: "db_tables",
        description: "List the tables (and views) a data source exposes, with each table's columns and types. Only tables in the source's allowed_tables are returned. Use this before db_select to learn the schema.",
        params: &[ParamDef {
            name: "source",
            description: "Data source name, as returned by db_sources",
            required: true,
        }],
    },
    ToolDef {
        name: "db_select",
        description: "Read rows from one allowed table. Identifiers are validated, every value is bound as a parameter, the query is read-only. truncated=true means the source held more rows than max_rows allows.",
        params: &[
            ParamDef {
                name: "source",
                description: "Data source name, as returned by db_sources",
                required: true,
            },
            ParamDef {
                name: "table",
                description: "Table name (must be in the source's allowed_tables)",
                required: true,
            },
            ParamDef {
                name: "columns",
                description: "JSON array of column names, e.g. [\"id\",\"name\"]. Omit for all columns.",
                required: false,
            },
            ParamDef {
                name: "filter",
                description: "JSON array of conditions, e.g. [{\"column\":\"name\",\"op\":\"=\",\"value\":\"Amy\"}]. op is one of = != < <= > >= like in (in takes an array value).",
                required: false,
            },
            ParamDef {
                name: "order_by",
                description: "Column name, optionally followed by asc or desc",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max rows to return (capped by the source's max_rows)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "db_query",
        description: "Run a read-only SELECT/WITH query against a data source. Only allowed when the source's allowed_tables is exactly [\"*\"]; elsewhere use db_select. Single statement, read-only transaction.",
        params: &[
            ParamDef {
                name: "source",
                description: "Data source name, as returned by db_sources",
                required: true,
            },
            ParamDef {
                name: "sql",
                description: "A single read-only SQL statement (SELECT / WITH)",
                required: true,
            },
            ParamDef {
                name: "limit",
                description: "Max rows to return (capped by the source's max_rows)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "file_read",
        description: "Read a plain-text file (txt/md/json/log/yaml) from an allowed directory. Use this instead of Read — only this route passes de-identification. Max 512 KiB, CJK-safe cut. CSV/spreadsheets are refused.",
        params: &[
            ParamDef {
                name: "path",
                description: "Absolute path to the file",
                required: true,
            },
            ParamDef {
                name: "max_bytes",
                description: "Max bytes to read (default and maximum 524288)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "csv_read",
        description: "Read a CSV/TSV file as structured rows. Use this instead of Read or a shell command: only this route passes de-identification. Returns {path, table, columns, rows, row_count, truncated}. Max 64 MiB.",
        params: &[
            ParamDef {
                name: "path",
                description: "Absolute path to the .csv / .tsv file",
                required: true,
            },
            ParamDef {
                name: "delimiter",
                description: "Single character, or \\t for tab. Default ,",
                required: false,
            },
            ParamDef {
                name: "has_header",
                description: "true (default) treats row 1 as the header; false synthesizes c1..cN",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max data rows to return (default 200, maximum 2000)",
                required: false,
            },
            ParamDef {
                name: "offset",
                description: "Data rows to skip before returning (default 0)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "xlsx_read",
        description: "Read one worksheet of an Excel/ODF workbook (xlsx/xlsm/xls/ods) as rows. Use this instead of Read or a shell command — only this route passes de-identification. Row 1 is the header. Limit 32 MiB.",
        params: &[
            ParamDef {
                name: "path",
                description: "Absolute path to the workbook",
                required: true,
            },
            ParamDef {
                name: "sheet",
                description: "Worksheet name. Omit for the first sheet; `sheets` in the result lists them all.",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Max data rows to return (default 200, maximum 2000)",
                required: false,
            },
            ParamDef {
                name: "offset",
                description: "Data rows to skip before returning (default 0)",
                required: false,
            },
        ],
    },
];
