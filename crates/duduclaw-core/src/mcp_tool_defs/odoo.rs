//! Odoo ERP bridge (CRM / Sales / Inventory / Accounting).
//!
//! Part of the split `tools_def` registration face (O7). Every `description`
//! here is bounded by the O7 budget (<=200 bytes for a tool, <=200 bytes for a
//! parameter) and enforced by `mcp::tools_list_budget_tests` — long prose
//! belongs in `docs/guides/mcp-tools.md`, not on the wire.

use super::{ParamDef, ToolDef};

pub(super) const TOOLS: &[ToolDef] = &[
    ToolDef {
        name: "odoo_connect",
        description: "Connect to Odoo ERP and authenticate. Must be called before using other odoo_* tools.",
        params: &[],
    },
    ToolDef {
        name: "odoo_status",
        description: "Show Odoo connection status, version, edition (CE/EE), and installed modules",
        params: &[],
    },
    ToolDef {
        name: "odoo_crm_leads",
        description: "Search CRM leads/opportunities in Odoo",
        params: &[
            ParamDef {
                name: "stage",
                description: "Filter by stage name (optional)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_crm_create_lead",
        description: "Create a new CRM lead in Odoo",
        params: &[
            ParamDef {
                name: "name",
                description: "Lead name / subject",
                required: true,
            },
            ParamDef {
                name: "contact_name",
                description: "Contact person name",
                required: false,
            },
            ParamDef {
                name: "email",
                description: "Contact email",
                required: false,
            },
            ParamDef {
                name: "phone",
                description: "Contact phone",
                required: false,
            },
            ParamDef {
                name: "expected_revenue",
                description: "Positive number or numeric string; decimals allowed",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_crm_update_stage",
        description: "Move a CRM lead to a different stage",
        params: &[
            ParamDef {
                name: "lead_id",
                description: "Lead ID: positive integer or numeric string",
                required: true,
            },
            ParamDef {
                name: "stage_name",
                description: "Target stage name",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "odoo_sale_orders",
        description: "Search sale orders in Odoo",
        params: &[
            ParamDef {
                name: "status",
                description: "Filter by status (draft/sale/done)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_sale_create_quotation",
        description: "Create a new quotation (draft sale order) in Odoo",
        params: &[
            ParamDef {
                name: "partner_id",
                description: "Customer partner ID: positive integer or numeric string",
                required: true,
            },
            ParamDef {
                name: "product_id",
                description: "Product ID: positive integer or numeric string",
                required: true,
            },
            ParamDef {
                name: "quantity",
                description: "Positive number or numeric string (default 1); decimals allowed",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_sale_confirm",
        description: "Confirm a quotation into a sale order",
        params: &[ParamDef {
            name: "order_id",
            description: "Sale order ID to confirm: positive integer or numeric string",
            required: true,
        }],
    },
    ToolDef {
        name: "odoo_inventory_products",
        description: "Search all product types with stock levels; optionally restrict to storable products",
        params: &[
            ParamDef {
                name: "storable_only",
                description: "Boolean (default false). Only storable products when true; uses the server's available stockable field",
                required: false,
            },
            ParamDef {
                name: "query",
                description: "Product name search",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_inventory_check",
        description: "Return product name, on-hand/forecast totals, and up to 10 stock location details",
        params: &[ParamDef {
            name: "product_id",
            description: "Product ID: positive integer or numeric string",
            required: true,
        }],
    },
    ToolDef {
        name: "odoo_invoice_list",
        description: "List invoices from Odoo (draft/posted/paid)",
        params: &[
            ParamDef {
                name: "status",
                description: "Filter: draft/posted/paid",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_payment_status",
        description: "Check payment status for an invoice",
        params: &[ParamDef {
            name: "invoice_id",
            description: "Invoice ID: positive integer or numeric string",
            required: true,
        }],
    },
    ToolDef {
        name: "odoo_search",
        description: "Generic Odoo model search (advanced). Blocked models: ir.config_parameter, res.users, ir.cron, etc.",
        params: &[
            ParamDef {
                name: "model",
                description: "Odoo model name (e.g. res.partner)",
                required: true,
            },
            ParamDef {
                name: "domain",
                description: "Search domain as JSON array",
                required: false,
            },
            ParamDef {
                name: "fields",
                description: "Comma-separated field names",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 20, max 100)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_execute",
        description: "Call a method on an Odoo model (advanced). Example: action_confirm on sale.order.",
        params: &[
            ParamDef {
                name: "model",
                description: "Odoo model name",
                required: true,
            },
            ParamDef {
                name: "method",
                description: "Method name to call",
                required: true,
            },
            ParamDef {
                name: "ids",
                description: "Record IDs as JSON array",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "odoo_report",
        description: "Generate a PDF report from Odoo (e.g. invoice, quotation)",
        params: &[
            ParamDef {
                name: "report_name",
                description: "Report template name (e.g. account.report_invoice)",
                required: true,
            },
            ParamDef {
                name: "record_id",
                description: "Record ID: positive integer or numeric string",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "odoo_partner_search",
        description: "Search customers/contacts in Odoo (res.partner). Read-only, returns only non-sensitive fields (name, email, phone, city, company flag, ref). Use it to find a partner_id before creating a quotation.",
        params: &[
            ParamDef {
                name: "query",
                description: "Name / email / customer ref to match (fuzzy)",
                required: false,
            },
            ParamDef {
                name: "limit",
                description: "Positive integer or numeric string (default 10, max 40)",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_schema_overview",
        description: "Browse the scanned Odoo structure within agent permissions. Optional model returns fields; query searches names and labels. x_* fields are marked custom. No business rows.",
        params: &[
            ParamDef {
                name: "model",
                description: "Optional exact model name to inspect its scanned fields",
                required: false,
            },
            ParamDef {
                name: "query",
                description: "Optional case-insensitive substring in model/field names and labels",
                required: false,
            },
        ],
    },
    ToolDef {
        name: "odoo_schema_fields",
        description: "Inspect one Odoo model's field structure (fields_get). Returns field metadata only (name, type, label, required, relation) — no records. Use it to learn a custom model's columns before querying.",
        params: &[ParamDef {
            name: "model",
            description: "Odoo model name (e.g. x_custom_model, res.partner)",
            required: true,
        }],
    },
    // RM-120: listed only when the employee's Odoo settings explicitly grant
    // `create` / `write` on at least one model (`mcp::tools_list`).
    ToolDef {
        name: "odoo_create",
        description: "Create one record in an Odoo model granted in the employee's Odoo settings. Keys must be writable fields of the scanned schema. Returns the new ID.",
        params: &[
            ParamDef {
                name: "model",
                description: "Odoo model name (e.g. crm.lead)",
                required: true,
            },
            ParamDef {
                name: "values",
                description: "Field values object, max 64 KiB. x2many fields take command lists [0,0,{..}] / [1,id,{..}] / [4,id] only (no 2/3/5/6, false or plain id lists); active cannot be set false",
                required: true,
            },
        ],
    },
    ToolDef {
        name: "odoo_write",
        description: "Update records in an Odoo model granted in the employee's Odoo settings. Keys must be writable fields of the scanned schema. Cannot delete records.",
        params: &[
            ParamDef {
                name: "model",
                description: "Odoo model name (e.g. crm.lead)",
                required: true,
            },
            ParamDef {
                name: "ids",
                description: "Record IDs: 1 to 100 positive integers",
                required: true,
            },
            ParamDef {
                name: "values",
                description: "Field values object, max 64 KiB. x2many fields take command lists [0,0,{..}] / [1,id,{..}] / [4,id] only (no 2/3/5/6, false or plain id lists); active cannot be set false",
                required: true,
            },
        ],
    },
];
