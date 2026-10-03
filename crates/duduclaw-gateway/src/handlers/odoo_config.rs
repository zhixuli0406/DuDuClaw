//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// Render the compact, auto-injectable Odoo schema summary (wiki `context`
/// layer). Lists custom (`x_`) models in full and caps the business-model roll
/// so the auto-injected prompt stays small.
pub(crate) fn render_odoo_schema_summary(report: &duduclaw_odoo::SchemaReport) -> String {
    const MAX_BUSINESS_ROWS: usize = 60;
    let custom: Vec<&duduclaw_odoo::SchemaModel> =
        report.models.iter().filter(|m| m.custom).collect();
    let business: Vec<&duduclaw_odoo::SchemaModel> =
        report.models.iter().filter(|m| !m.custom).collect();

    let mut s = String::new();
    s.push_str("---\ntitle: Odoo 資料表結構摘要\nlayer: context\ntags: [odoo, schema]\n---\n\n");
    s.push_str("# Odoo 資料表結構摘要\n\n");
    s.push_str(&format!(
        "掃描到 {} 個資料表（顯示 {} 個{}）。完整欄位表見 wiki 頁 `odoo/schema-fields`，或用 `odoo_schema_fields` 工具查單一資料表。\n\n",
        report.total_models,
        report.models.len(),
        if report.truncated { "，已截斷" } else { "" },
    ));

    if !custom.is_empty() {
        s.push_str("## 自訂資料表 (x_)\n\n");
        for m in &custom {
            s.push_str(&format!(
                "- `{}` — {} ({} 欄)\n",
                m.model, m.name, m.field_count
            ));
        }
        s.push('\n');
    }

    s.push_str("## 內建資料表\n\n");
    for m in business.iter().take(MAX_BUSINESS_ROWS) {
        s.push_str(&format!(
            "- `{}` — {} ({} 欄)\n",
            m.model, m.name, m.field_count
        ));
    }
    if business.len() > MAX_BUSINESS_ROWS {
        s.push_str(&format!(
            "- …其餘 {} 個內建資料表見 `odoo/schema-fields`\n",
            business.len() - MAX_BUSINESS_ROWS
        ));
    }
    s
}

/// Render the full per-model field tables (wiki `deep` layer, search-only).
/// Pre-truncated so it comfortably fits under `WikiStore`'s page-size cap.
pub(crate) fn render_odoo_schema_details(report: &duduclaw_odoo::SchemaReport) -> String {
    const MAX_BYTES: usize = 200_000;
    let mut s = String::new();
    s.push_str(
        "---\ntitle: Odoo 資料表欄位明細\nlayer: deep\ntags: [odoo, schema, fields]\n---\n\n",
    );
    s.push_str("# Odoo 資料表欄位明細\n\n");
    for m in &report.models {
        let custom_tag = if m.custom { " (自訂)" } else { "" };
        s.push_str(&format!("## `{}` — {}{}\n\n", m.model, m.name, custom_tag));
        for f in &m.fields {
            let req = if f.required { " [required]" } else { "" };
            let rel = f
                .relation
                .as_deref()
                .map(|r| format!(" → {r}"))
                .unwrap_or_default();
            s.push_str(&format!(
                "- `{}`: {}{}{} — {}\n",
                f.name, f.ttype, rel, req, f.label
            ));
        }
        s.push('\n');
        if s.len() > MAX_BYTES {
            s.push_str("\n_…輸出過長已截斷，請用 `odoo_schema_fields` 查詢個別資料表。_\n");
            break;
        }
    }
    s
}

// ── P2 ODO helpers (per-agent [odoo] override) ────────────────────────────────

/// Validate a per-agent Odoo `allowed_actions` entry. Accepts a bare verb
/// (`read`/`write`/`create`/`unlink`/`execute`) or a qualified `verb:model`
/// form (e.g. `write:crm.lead`). The model part is validated like an Odoo model
/// name (alphanumeric + `.` + `_`).
pub(crate) fn odo_valid_action(action: &str) -> bool {
    const VERBS: &[&str] = &["read", "write", "create", "unlink", "execute"];
    let (verb, model) = match action.split_once(':') {
        Some((v, m)) => (v, Some(m)),
        None => (action, None),
    };
    if !VERBS.contains(&verb) {
        return false;
    }
    match model {
        None => true,
        Some(m) => {
            !m.is_empty()
                && m.len() <= 128
                && m.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '_')
        }
    }
}

/// Apply the per-agent `[odoo]` override fields from `agents.update` params.
/// Encrypts `api_key`/`password` into their `_enc` variants — cleartext is
/// never written. Returns the change list (empty if no `odoo` object present).
pub(crate) fn apply_odoo_to_table(
    table: &mut toml::Table,
    params: &Value,
    home_dir: &Path,
) -> Result<Vec<String>, String> {
    let mut changes: Vec<String> = Vec::new();
    let odoo_in = match params.get("odoo").and_then(|v| v.as_object()) {
        Some(o) => o,
        None => return Ok(changes),
    };

    let section = table
        .entry("odoo")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or("Invalid [odoo] section")?;

    // profile (string)
    if let Some(v) = odoo_in.get("profile").and_then(|v| v.as_str()) {
        let v = v.trim();
        if v.is_empty() {
            section.remove("profile");
            changes.push("odoo.profile cleared".into());
        } else if v.len() <= 64
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            section.insert("profile".into(), toml::Value::String(v.into()));
            changes.push(format!("odoo.profile = \"{v}\""));
        } else {
            return Err("odoo.profile must be ≤64 chars of [a-zA-Z0-9_-]".into());
        }
    }

    // allowed_models[] (Odoo model names)
    if let Some(arr) = odoo_in.get("allowed_models").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let m = item.as_str().unwrap_or("").trim();
            if m.is_empty() {
                continue;
            }
            if !MethodHandler::is_valid_odoo_model(m) {
                return Err(format!("Invalid odoo allowed_models entry '{m}'"));
            }
            out.push(toml::Value::String(m.into()));
        }
        section.insert("allowed_models".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.allowed_models = [{} entries]", out.len()));
    }

    // unblock_models[] (opt-out of the built-in security block list)
    if let Some(arr) = odoo_in.get("unblock_models").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let m = item.as_str().unwrap_or("").trim();
            if m.is_empty() {
                continue;
            }
            if !MethodHandler::is_valid_odoo_model(m) {
                return Err(format!("Invalid odoo unblock_models entry '{m}'"));
            }
            out.push(toml::Value::String(m.into()));
        }
        section.insert("unblock_models".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.unblock_models = [{} entries]", out.len()));
    }

    // allowed_actions[] (bare verb or verb:model)
    if let Some(arr) = odoo_in.get("allowed_actions").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let a = item.as_str().unwrap_or("").trim();
            if a.is_empty() {
                continue;
            }
            if !odo_valid_action(a) {
                return Err(format!(
                    "Invalid odoo allowed_actions entry '{a}' (expected verb or verb:model, \
                     e.g. 'read' or 'write:crm.lead')"
                ));
            }
            out.push(toml::Value::String(a.into()));
        }
        section.insert("allowed_actions".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.allowed_actions = [{} entries]", out.len()));
    }

    // company_ids[] (ints)
    if let Some(arr) = odoo_in.get("company_ids").and_then(|v| v.as_array()) {
        let mut out: Vec<toml::Value> = Vec::new();
        for item in arr {
            let n = item
                .as_i64()
                .ok_or("odoo company_ids entries must be integers")?;
            if n < 0 {
                return Err("odoo company_ids must be non-negative".into());
            }
            out.push(toml::Value::Integer(n));
        }
        section.insert("company_ids".into(), toml::Value::Array(out.clone()));
        changes.push(format!("odoo.company_ids = [{} entries]", out.len()));
    }

    // v1.68: a stored credential only follows the URL it was entered for.
    // Changing `url` while keeping `api_key` / `password` (absent or a
    // placeholder) is refused, so the secret is never sent to a new host.
    if let Some(new_url) = odoo_in.get("url").and_then(|v| v.as_str()).map(str::trim)
        && !new_url.is_empty()
        && section.get("url").and_then(|v| v.as_str()) != Some(new_url)
    {
        for (param_key, enc_key) in [("api_key", "api_key_enc"), ("password", "password_enc")] {
            let stored = section.contains_key(enc_key) || section.contains_key(param_key);
            let kept = odoo_in
                .get(param_key)
                .and_then(|v| v.as_str())
                .is_none_or(|v| super::config_commit::is_secret_placeholder(v.trim()));
            if stored && kept {
                return Err(format!(
                    "odoo.url changed — re-enter odoo.{param_key} for the new server (or send \"\" to clear it)"
                ));
            }
        }
    }

    // url / db / username (plaintext scalars, optional overrides).
    // url + db are validated with the SAME SSRF / db-name validators as the
    // global `odoo.configure` path — an override must not be a bypass (I5).
    for (param_key, toml_key) in &[("url", "url"), ("db", "db"), ("username", "username")] {
        if let Some(v) = odoo_in.get(*param_key).and_then(|v| v.as_str()) {
            let v = v.trim();
            if v.is_empty() {
                section.remove(*toml_key);
                changes.push(format!("odoo.{toml_key} cleared"));
            } else {
                if *toml_key == "url" && !MethodHandler::is_safe_odoo_url(v) {
                    return Err(
                        "odoo.url must use HTTPS (http:// only allowed for localhost/127.0.0.1) \
                         and must not target a private/reserved host"
                            .into(),
                    );
                }
                if *toml_key == "db"
                    && !(v.len() < 64
                        && v.chars()
                            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                {
                    return Err("odoo.db must be ≤63 chars of [a-zA-Z0-9_-]".into());
                }
                section.insert((*toml_key).into(), toml::Value::String(v.into()));
                changes.push(format!("odoo.{toml_key} = [SET]"));
            }
        }
    }

    // api_key / password → encrypt to *_enc, never store cleartext.
    for (param_key, enc_key) in &[("api_key", "api_key_enc"), ("password", "password_enc")] {
        if let Some(v) = odoo_in.get(*param_key).and_then(|v| v.as_str()) {
            // Refuse to persist the masked placeholder back as a real secret.
            if super::config_commit::is_secret_placeholder(v) {
                continue;
            }
            // Drop any stale cleartext mirror.
            section.remove(*param_key);
            if v.is_empty() {
                section.remove(*enc_key);
                changes.push(format!("odoo.{param_key} cleared"));
            } else if v.starts_with("secret://") {
                // A `secret://` reference is a POINTER, not a secret to be
                // encrypted. Store it RAW into `*_enc` (the field
                // merge_credentials reads) so the connector pool can resolve it
                // via the SecretManager at connect time.
                section.insert((*enc_key).into(), toml::Value::String(v.into()));
                changes.push(format!("odoo.{param_key} = [SECRET REF]"));
            } else if let Some(enc) = crate::config_crypto::encrypt_value(v, home_dir) {
                section.insert((*enc_key).into(), toml::Value::String(enc));
                changes.push(format!("odoo.{param_key} = [ENCRYPTED]"));
            } else {
                return Err(format!("Failed to encrypt odoo.{param_key}"));
            }
        }
    }

    Ok(changes)
}

#[cfg(test)]
mod v168_url_binding_tests {
    use super::*;

    #[test]
    fn kept_credential_cannot_follow_a_new_url() {
        let home = tempfile::tempdir().unwrap();
        let mut t: toml::Table =
            toml::from_str("[odoo]\nurl = \"https://erp.example.com\"\napi_key_enc = \"ENC\"\n").unwrap();
        let e = apply_odoo_to_table(&mut t, &json!({"odoo": {"url": "https://evil.example.com"}}), home.path())
            .unwrap_err();
        assert!(e.contains("odoo.api_key"), "{e}");
        assert!(apply_odoo_to_table(
            &mut t,
            &json!({"odoo": {"url": "https://evil.example.com", "api_key": "«set»"}}),
            home.path()
        )
        .is_err());
        // Same URL re-sent with the placeholder is fine.
        apply_odoo_to_table(&mut t, &json!({"odoo": {"url": "https://erp.example.com", "api_key": "***set***"}}), home.path())
            .unwrap();
        assert_eq!(t["odoo"]["api_key_enc"].as_str(), Some("ENC"));
    }
}
