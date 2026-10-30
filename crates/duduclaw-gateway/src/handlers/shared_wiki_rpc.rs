//! Split out of the pre-split `handlers.rs` (P12b). Pure movement.

#[allow(unused_imports)]
use super::*;

/// One department gate shared by dashboard RPCs and private exports. Human
/// departments come from `users.department`, never from an AI employee or JWT.
pub(crate) struct SharedWikiReader {
    admin: bool,
    department: Option<String>,
    policy: duduclaw_core::DepartmentVisibilityPolicy,
}

impl SharedWikiReader {
    pub(crate) fn new(home: &Path, ctx: &UserContext) -> Result<Self, ()> {
        let live = super::task_privacy::live_reader_context(home, ctx)?;
        if live.requires_password_change() {
            return Err(());
        }
        if live.is_admin() {
            return Ok(Self {
                admin: true,
                department: None,
                policy: duduclaw_core::DepartmentVisibilityPolicy::empty(),
            });
        }
        let db_path = home.join("users.db");
        if !db_path.is_file() {
            return Err(());
        }
        let db = crate::review_evidence::audience::readonly_db(&db_path).map_err(|_| ())?;
        let department: Option<String> = db.query_row(
            "SELECT department FROM users WHERE id = ?1 AND status = 'active' AND must_change_password = 0",
            [&live.user_id],
            |row| row.get(0),
        ).map_err(|_| ())?;
        let path = home.join("shared/wiki/.scope.toml");
        let policy = match std::fs::read_to_string(&path) {
            Ok(raw) => {
                // The agent parser is deliberately lenient. Dashboard reads
                // fail closed if a declared visibility rule has a bad shape.
                let table: toml::Table = raw.parse().map_err(|_| ())?;
                if let Some(namespaces) = table.get("namespaces") {
                    for entry in namespaces.as_table().ok_or(())?.values() {
                        let entry = entry.as_table().ok_or(())?;
                        if let Some(departments) = entry.get("visible_to_departments") {
                            if !departments.as_array().ok_or(())?.iter().all(|v| v.is_str()) {
                                return Err(());
                            }
                        }
                    }
                }
                duduclaw_core::DepartmentVisibilityPolicy::parse(&raw).map_err(|_| ())?
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !path.is_symlink() => {
                duduclaw_core::DepartmentVisibilityPolicy::empty()
            }
            Err(_) => return Err(()),
        };
        Ok(Self { admin: false, department, policy })
    }

    pub(crate) fn page_visible(&self, path: &str) -> bool {
        if self.admin {
            return true;
        }
        // Normalise by refusing alternate spellings before applying the
        // namespace rule. Global indexes/logs can name hidden department pages.
        if path.contains('\\')
            || path.split('/').any(|part| part.is_empty() || part == "." || part == "..")
            || path.split('/').any(|part| part.starts_with('_') || part.starts_with('.'))
        {
            return false;
        }
        self.policy.page_visible(path, self.department.as_deref())
    }
}

impl MethodHandler {
    // ── Shared Wiki ─────────────────────────────────────────

    pub(crate) async fn handle_shared_wiki_pages(&self, ctx: &UserContext) -> WsFrame {
        let reader = match SharedWikiReader::new(&self.home_dir, ctx) {
            Ok(reader) => reader,
            Err(()) => return WsFrame::error_response("", "permission denied"),
        };
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "pages": [], "exists": false }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.list_pages() {
            Ok(pages) => {
                let items: Vec<Value> = pages
                    .iter()
                    .filter(|p| reader.page_visible(&p.path))
                    .map(|p| {
                        json!({
                            "path": p.path,
                            "title": p.title,
                            "updated": p.updated.to_rfc3339(),
                            "tags": p.tags,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "pages": items, "exists": true }))
            }
            Err(e) => {
                WsFrame::error_response("", &format!("Failed to list shared wiki pages: {e}"))
            }
        }
    }

    pub(crate) async fn handle_shared_wiki_read(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let reader = match SharedWikiReader::new(&self.home_dir, ctx) {
            Ok(reader) => reader,
            Err(()) => return WsFrame::error_response("", "permission denied"),
        };
        let page_path = params
            .get("page_path")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if page_path.is_empty() {
            return WsFrame::error_response("", "Missing 'page_path' parameter");
        }

        if !reader.page_visible(page_path) {
            return WsFrame::error_response("", "permission denied");
        }
        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        match store.read_raw(page_path) {
            Ok(content) => {
                WsFrame::ok_response("", json!({ "content": content, "path": page_path }))
            }
            Err(e) => WsFrame::error_response("", &format!("Failed to read shared wiki page: {e}")),
        }
    }

    pub(crate) async fn handle_shared_wiki_search(&self, params: Value, ctx: &UserContext) -> WsFrame {
        let reader = match SharedWikiReader::new(&self.home_dir, ctx) {
            Ok(reader) => reader,
            Err(()) => return WsFrame::error_response("", "permission denied"),
        };
        let query = params.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let limit = (params.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as usize).min(100);
        let agent_id = params
            .get("agent_id")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let conversation_id = params
            .get("conversation_id")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty());

        if query.is_empty() {
            return WsFrame::error_response("", "Missing 'query' parameter");
        }

        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "hits": [] }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let result = match (conversation_id, !agent_id.is_empty()) {
            (Some(conv_id), true) if reader.admin => {
                let tracker = duduclaw_memory::feedback::global_tracker();
                store.search_with_citation(query, limit, agent_id, conv_id, None, &tracker)
            }
            // Filter before applying the caller's limit, so hidden matches do
            // not crowd their department's results out of the response.
            _ => store.search(query, if reader.admin { limit } else { usize::MAX }),
        };
        match result {
            Ok(hits) => {
                let items: Vec<Value> = hits
                    .iter()
                    .filter(|h| reader.page_visible(&h.path))
                    .take(limit)
                    .map(|h| {
                        json!({
                            "path": h.path,
                            "title": h.title,
                            "score": h.score,
                            "weighted_score": h.weighted_score,
                            "trust": h.trust,
                            "layer": h.layer.to_string(),
                            "source_type": h.source_type.to_string(),
                            "context_lines": h.context_lines,
                        })
                    })
                    .collect();
                WsFrame::ok_response("", json!({ "hits": items }))
            }
            Err(e) => WsFrame::error_response("", &format!("Shared wiki search failed: {e}")),
        }
    }

    pub(crate) async fn handle_shared_wiki_stats(&self, ctx: &UserContext) -> WsFrame {
        let reader = match SharedWikiReader::new(&self.home_dir, ctx) {
            Ok(reader) => reader,
            Err(()) => return WsFrame::error_response("", "permission denied"),
        };
        let wiki_dir = self.home_dir.join("shared").join("wiki");
        if !wiki_dir.exists() {
            return WsFrame::ok_response("", json!({ "exists": false, "total_pages": 0 }));
        }

        let store = duduclaw_memory::WikiStore::new_shared(&self.home_dir);
        let pages = match store.list_pages() {
            Ok(p) => p.into_iter().filter(|p| reader.page_visible(&p.path)).collect::<Vec<_>>(),
            Err(e) => {
                return WsFrame::error_response(
                    "",
                    &format!("Failed to list shared wiki pages: {e}"),
                );
            }
        };

        let mut by_author: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        let mut by_dir: std::collections::HashMap<String, usize> = std::collections::HashMap::new();

        for p in &pages {
            // Count by author from the WikiPage.author field
            let author = p.author.as_deref().unwrap_or("unknown");
            *by_author.entry(author.to_string()).or_default() += 1;

            let dir = std::path::Path::new(&p.path)
                .parent()
                .and_then(|d| d.to_str())
                .unwrap_or("root")
                .to_string();
            *by_dir.entry(dir).or_default() += 1;
        }

        let most_recent = pages.first().map(|p| {
            json!({
                "title": p.title,
                "path": p.path,
                "updated": p.updated.to_rfc3339(),
                "author": p.author,
            })
        });

        WsFrame::ok_response(
            "",
            json!({
                "exists": true,
                "total_pages": pages.len(),
                "by_author": by_author,
                "by_directory": by_dir,
                "most_recent": most_recent,
            }),
        )
    }
}

#[cfg(test)]
mod visibility_tests {
    use super::*;

    fn payload(frame: WsFrame) -> Value {
        match frame {
            WsFrame::Response { ok: true, payload: Some(payload), .. } => payload,
            other => panic!("expected success: {other:?}"),
        }
    }

    fn denied(frame: WsFrame) {
        assert!(matches!(frame, WsFrame::Response { ok: false, .. }));
    }

    #[tokio::test]
    async fn employee_shared_wiki_reads_use_the_humans_live_department() {
        let home = tempfile::tempdir().unwrap();
        let db = Arc::new(UserDb::new(&home.path().join("users.db")).unwrap());
        let user = db.create_user("reader@test.invalid", "Reader", "test-password", UserRole::Employee).unwrap();
        db.set_department(&user.id, Some("sales")).unwrap();
        let ctx = UserContext {
            user_id: user.id.clone(),
            email: user.email.clone(),
            role: UserRole::Employee,
            agent_access: HashMap::new(),
            team_access: HashMap::new(),
            must_change_password: false,
            acl_generation: 0,
        };
        let handler = MethodHandler::new(home.path().to_path_buf()).await;
        handler.set_user_db(db.clone(), Arc::new(JwtConfig::new(b"test-secret-at-least-32-bytes-long!!"))).await;
        handler.set_edition_override(Some(duduclaw_core::EditionProfile::Enterprise)).await;
        let store = duduclaw_memory::WikiStore::new_shared(home.path());
        store.ensure_scaffold().unwrap();
        for path in ["company.md", "departments/sales/own.md", "departments/hr/secret.md", "legal/secret.md"] {
            store.write_page(path, "# Handbook\nhandbook content").unwrap();
        }
        std::fs::write(home.path().join("shared/wiki/.scope.toml"), "[namespaces.legal]\nvisible_to_departments = [\"hr\"]\n").unwrap();

        let list = payload(handler.handle("shared_wiki.pages", json!({}), &ctx).await);
        assert_eq!(list["pages"].as_array().unwrap().len(), 2);
        for path in ["departments/hr/secret.md", "legal/secret.md", "_index.md", "./departments/hr/secret.md"] {
            denied(handler.handle("shared_wiki.read", json!({"page_path": path}), &ctx).await);
        }
        let hits = payload(handler.handle("shared_wiki.search", json!({"query": "handbook"}), &ctx).await);
        assert_eq!(hits["hits"].as_array().unwrap().len(), 2);
        assert!(hits["hits"].as_array().unwrap().iter().all(|hit| {
            matches!(hit["path"].as_str(), Some("company.md" | "departments/sales/own.md"))
        }));
        let stats = payload(handler.handle("shared_wiki.stats", json!({}), &ctx).await);
        assert_eq!(stats["total_pages"], 2);
        assert!(stats["by_directory"].get("departments/hr").is_none());
        payload(handler.handle("shared_wiki.read", json!({"page_path": "departments/sales/own.md"}), &ctx).await);

        // The same logged-in account loses department visibility immediately.
        db.set_department(&user.id, None).unwrap();
        denied(handler.handle("shared_wiki.read", json!({"page_path": "departments/sales/own.md"}), &ctx).await);
        let list = payload(handler.handle("shared_wiki.pages", json!({}), &ctx).await);
        assert_eq!(list["pages"].as_array().unwrap().len(), 1);
        assert_eq!(list["pages"][0]["path"], "company.md");
        denied(handler.handle("wiki.share", json!({"agent_id": "sales", "page_path": "auto/test.md"}), &ctx).await);

        for raw in ["[broken", "[namespaces.legal]\nvisible_to_departments = true\n"] {
            std::fs::write(home.path().join("shared/wiki/.scope.toml"), raw).unwrap();
            denied(handler.handle("shared_wiki.read", json!({"page_path": "company.md"}), &ctx).await);
            denied(handler.handle("shared_wiki.pages", json!({}), &ctx).await);
        }
    }
}
