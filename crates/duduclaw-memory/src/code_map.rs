//! Aider-style code symbol graph (`code_map`).
//!
//! Reuses the HippoRAG-lite Personalized PageRank engine from [`graph_rank`]
//! ([`TripleGraph`]) to rank a repository's source files by relevance to a
//! natural-language / identifier query — the same idea as Aider's RepoMap
//! (tree-sitter symbol extraction + PageRank over a symbol graph), but built on
//! the PPR engine this crate already ships (damping `d = 0.5`, ≤20 iterations).
//!
//! ## How the graph maps onto [`TripleGraph`]
//!
//! `TripleGraph` links *memory nodes* to *entity nodes* via `(memory_id,
//! subject, object)` rows. Here the mapping is:
//!
//! - **memory node** = a source file (its repo-relative path)
//! - **entity node** = a symbol name (function / struct / class / …)
//! - one `(file, symbol, None)` row per (file, symbol) relationship
//!
//! Edge *weight* is expressed through parallel edges: a **definition** emits
//! [`W_DEF`] identical rows, a cross-file **reference** emits [`W_REF`]. So a
//! file that *defines* `foo` binds to the `foo` entity four times as strongly
//! as a file that merely *uses* it — mirroring Aider's higher weight on
//! definitions.
//!
//! ## Reference edges are def-filtered (noise control)
//!
//! We do not ship per-language tag queries. Instead, references are the subset
//! of a file's identifier tokens that are **defined somewhere else in the
//! repo**. Local variables and parameters almost never collide with a global
//! definition name, so this cheaply approximates Aider's def/ref distinction
//! without `.scm` files, and keeps the graph to cross-file symbol usage — which
//! is exactly what makes PageRank meaningful.
//!
//! Fail-safe: an empty repo, an unparsable file, or a query matching no symbol
//! all degrade to an empty ranking rather than an error.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tree_sitter::{Node, Parser};

use crate::graph_rank::{TripleGraph, TripleRow};
use duduclaw_core::error::{DuDuClawError, Result};

/// Parallel-edge weight for a symbol *definition*.
pub const W_DEF: usize = 4;
/// Parallel-edge weight for a cross-file symbol *reference*.
pub const W_REF: usize = 1;
/// Default cap on per-file size we will parse (bytes). Larger files are skipped
/// to bound parse time and memory.
pub const DEFAULT_MAX_FILE_BYTES: usize = 512 * 1024;
/// Hard cap on how many symbols we render per file in the text map.
pub const DEFAULT_SYMBOLS_PER_FILE: usize = 12;
/// Cap on a rendered signature line (bytes; walked back to a char boundary).
const SIGNATURE_MAX_BYTES: usize = 160;

/// Coarse symbol classification (best-effort across languages).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Class,
    Interface,
    TypeAlias,
    Module,
    Constant,
    Macro,
}

impl SymbolKind {
    fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Function => "fn",
            SymbolKind::Method => "method",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Trait => "trait",
            SymbolKind::Class => "class",
            SymbolKind::Interface => "interface",
            SymbolKind::TypeAlias => "type",
            SymbolKind::Module => "mod",
            SymbolKind::Constant => "const",
            SymbolKind::Macro => "macro",
        }
    }
}

/// A single defined symbol with its location and one-line signature.
#[derive(Debug, Clone, Serialize)]
pub struct SymbolInfo {
    pub name: String,
    pub kind: SymbolKind,
    /// 1-based line number of the definition.
    pub line: usize,
    /// First line of the definition (trimmed, byte-capped).
    pub signature: String,
}

/// Per-file extraction result.
#[derive(Debug, Clone)]
struct FileSymbols {
    /// Repo-relative path (forward slashes).
    path: String,
    defs: Vec<SymbolInfo>,
    /// Raw identifier tokens seen in the file (bag; deduped later against the
    /// global def set to form reference edges).
    idents: HashSet<String>,
}

/// A ranked source file with its most relevant defined symbols.
#[derive(Debug, Clone, Serialize)]
pub struct RankedFile {
    pub path: String,
    /// Normalized PPR score (top file = 1.0).
    pub score: f64,
    pub symbols: Vec<SymbolInfo>,
}

/// Configuration for building a [`CodeMap`].
#[derive(Debug, Clone)]
pub struct CodeMapConfig {
    /// Repository root to scan.
    pub root: PathBuf,
    /// Max file size to parse.
    pub max_file_bytes: usize,
    /// Extra include filter: if non-empty, only these extensions are scanned
    /// (lowercase, no dot). Empty = all supported languages.
    pub only_exts: Vec<String>,
}

impl CodeMapConfig {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            max_file_bytes: DEFAULT_MAX_FILE_BYTES,
            only_exts: Vec::new(),
        }
    }
}

/// The built symbol map for a repository.
pub struct CodeMap {
    files: Vec<FileSymbols>,
    /// Symbol name (normalized) → set of file indices that define it.
    def_owners: HashMap<String, Vec<usize>>,
    file_count: usize,
    symbol_count: usize,
}

/// Which tree-sitter language (and its def-node vocabulary) a file uses.
#[derive(Clone, Copy)]
enum Lang {
    Rust,
    Python,
    JavaScript,
    TypeScript,
    Tsx,
}

impl Lang {
    fn from_ext(ext: &str) -> Option<Lang> {
        match ext {
            "rs" => Some(Lang::Rust),
            "py" | "pyi" => Some(Lang::Python),
            "js" | "jsx" | "mjs" | "cjs" => Some(Lang::JavaScript),
            "ts" | "mts" | "cts" => Some(Lang::TypeScript),
            "tsx" => Some(Lang::Tsx),
            _ => None,
        }
    }

    fn language(self) -> tree_sitter::Language {
        match self {
            Lang::Rust => tree_sitter_rust::LANGUAGE.into(),
            Lang::Python => tree_sitter_python::LANGUAGE.into(),
            Lang::JavaScript => tree_sitter_javascript::LANGUAGE.into(),
            Lang::TypeScript => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            Lang::Tsx => tree_sitter_typescript::LANGUAGE_TSX.into(),
        }
    }

    /// Map a definition node kind to a [`SymbolKind`], or `None` if the node is
    /// not a definition we index.
    fn def_kind(self, node_kind: &str) -> Option<SymbolKind> {
        match self {
            Lang::Rust => match node_kind {
                "function_item" => Some(SymbolKind::Function),
                "struct_item" => Some(SymbolKind::Struct),
                "enum_item" => Some(SymbolKind::Enum),
                "union_item" => Some(SymbolKind::Struct),
                "trait_item" => Some(SymbolKind::Trait),
                "mod_item" => Some(SymbolKind::Module),
                "type_item" => Some(SymbolKind::TypeAlias),
                "const_item" | "static_item" => Some(SymbolKind::Constant),
                "macro_definition" => Some(SymbolKind::Macro),
                _ => None,
            },
            Lang::Python => match node_kind {
                "function_definition" => Some(SymbolKind::Function),
                "class_definition" => Some(SymbolKind::Class),
                _ => None,
            },
            Lang::JavaScript | Lang::TypeScript | Lang::Tsx => match node_kind {
                "function_declaration" | "generator_function_declaration" => {
                    Some(SymbolKind::Function)
                }
                "class_declaration" | "abstract_class_declaration" => Some(SymbolKind::Class),
                "method_definition" => Some(SymbolKind::Method),
                "interface_declaration" => Some(SymbolKind::Interface),
                "type_alias_declaration" => Some(SymbolKind::TypeAlias),
                "enum_declaration" => Some(SymbolKind::Enum),
                _ => None,
            },
        }
    }
}

/// Normalize a symbol name for graph-node identity (matches
/// `graph_rank::normalize_entity`: trim + lowercase).
fn normalize(name: &str) -> String {
    name.trim().to_lowercase()
}

/// Byte-safe first-line signature, capped at [`SIGNATURE_MAX_BYTES`].
fn signature_of(source: &str, node: &Node) -> String {
    let range = node.byte_range();
    let text = source.get(range).unwrap_or("");
    let first_line = text.lines().next().unwrap_or("").trim();
    duduclaw_core::truncate_bytes(first_line, SIGNATURE_MAX_BYTES).to_string()
}

impl CodeMap {
    /// Number of files with at least one indexed symbol.
    pub fn file_count(&self) -> usize {
        self.file_count
    }

    /// Total number of indexed definitions across all files.
    pub fn symbol_count(&self) -> usize {
        self.symbol_count
    }

    /// Build the symbol map by walking `config.root` (gitignore-aware,
    /// skipping `.git`, `target`, `node_modules`, etc. via the `ignore` crate).
    ///
    /// Files are read by [`read_source_nofollow`]: opened without following a
    /// final symlink (`O_NOFOLLOW` on Unix, the reparse point itself on
    /// Windows) and kept only when the opened descriptor is a regular,
    /// single-link file within the size cap. Callers that need a stronger
    /// containment guarantee (every directory component, mount crossings)
    /// pass their own reader to [`Self::build_with_reader`].
    pub fn build(config: &CodeMapConfig) -> Result<CodeMap> {
        crate::isolation_io::verify()?;
        Self::build_with_reader(config, |walked, _rel, max_bytes| {
            read_source_nofollow(walked, max_bytes)
        })
    }

    /// [`Self::build`] with the file reads delegated to `read`.
    ///
    /// `read(walked_path, path_relative_to_root, max_bytes)` returns the UTF-8
    /// source, or `None` to skip the file. The walk itself never follows a
    /// link and stays on the root's filesystem; it only proposes candidate
    /// paths — a file is indexed only if `read` returned its content, so the
    /// reader is the containment boundary. Content longer than
    /// `config.max_file_bytes` is dropped even if the reader returned it.
    pub fn build_with_reader<F>(config: &CodeMapConfig, mut read: F) -> Result<CodeMap>
    where
        F: FnMut(&Path, &Path, usize) -> Option<String>,
    {
        crate::isolation_io::verify()?;
        if !config.root.exists() {
            return Err(DuDuClawError::Memory(format!(
                "code_map root does not exist: {}",
                config.root.display()
            )));
        }
        let mut files: Vec<FileSymbols> = Vec::new();

        let walker = ignore::WalkBuilder::new(&config.root)
            .hidden(false) // still traverse dotfiles, but .gitignore rules apply
            .git_ignore(true)
            .git_global(false)
            .parents(true)
            .follow_links(false)
            .same_file_system(true)
            .build();

        for entry in walker.flatten() {
            crate::isolation_io::verify()?;
            // The entry's own type: with `follow_links(false)` a symlink
            // reports as a symlink, never as the file it points to.
            if !entry.file_type().is_some_and(|t| t.is_file()) {
                continue;
            }
            let path = entry.path();
            let ext = match path.extension().and_then(|e| e.to_str()) {
                Some(e) => e.to_lowercase(),
                None => continue,
            };
            if !config.only_exts.is_empty() && !config.only_exts.contains(&ext) {
                continue;
            }
            let lang = match Lang::from_ext(&ext) {
                Some(l) => l,
                None => continue,
            };
            let rel = match path.strip_prefix(&config.root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            crate::isolation_io::verify()?;
            let source = match read(path, rel, config.max_file_bytes) {
                Some(s) if s.len() <= config.max_file_bytes => s,
                _ => continue, // refused / binary / non-UTF8 / too large: skip
            };
            if let Some(fs) = extract_file(&config.root, path, lang, &source) {
                if !fs.defs.is_empty() || !fs.idents.is_empty() {
                    files.push(fs);
                }
            }
        }

        // Global definition ownership index.
        let mut def_owners: HashMap<String, Vec<usize>> = HashMap::new();
        let mut symbol_count = 0usize;
        for (fi, f) in files.iter().enumerate() {
            for d in &f.defs {
                symbol_count += 1;
                def_owners.entry(normalize(&d.name)).or_default().push(fi);
            }
        }
        let file_count = files.iter().filter(|f| !f.defs.is_empty()).count();

        Ok(CodeMap {
            files,
            def_owners,
            file_count,
            symbol_count,
        })
    }

    /// Build the `(file, symbol, None)` triple rows with parallel-edge weights.
    fn triple_rows(&self) -> Vec<TripleRow> {
        let mut rows: Vec<TripleRow> = Vec::new();
        for (fi, f) in self.files.iter().enumerate() {
            let mut local_defs: HashSet<String> = HashSet::new();
            // Definition edges (weight W_DEF).
            for d in &f.defs {
                let sym = normalize(&d.name);
                if sym.is_empty() {
                    continue;
                }
                local_defs.insert(sym.clone());
                for _ in 0..W_DEF {
                    rows.push((f.path.clone(), sym.clone(), None));
                }
            }
            // Reference edges (weight W_REF): identifiers this file uses that
            // are defined in *another* file.
            for ident in &f.idents {
                let sym = normalize(ident);
                if sym.is_empty() || local_defs.contains(&sym) {
                    continue;
                }
                let defined_elsewhere = self
                    .def_owners
                    .get(&sym)
                    .map(|owners| owners.iter().any(|&o| o != fi))
                    .unwrap_or(false);
                if defined_elsewhere {
                    for _ in 0..W_REF {
                        rows.push((f.path.clone(), sym.clone(), None));
                    }
                }
            }
        }
        rows
    }

    /// Rank files by relevance to `query`. `chat_files` (already-in-context
    /// files, repo-relative) get their defined symbols folded into the seed
    /// set — Aider's "files already in the chat" personalization. Returns at
    /// most `max_files` files, each with its top symbols.
    pub fn rank(&self, query: &str, chat_files: &[String], max_files: usize) -> Vec<RankedFile> {
        let rows = self.triple_rows();
        let graph = TripleGraph::from_triples(&rows);
        if graph.is_empty() {
            return Vec::new();
        }

        // Augment the query with the symbol names defined in chat files so they
        // seed the walk (reuses `seed_nodes`' whole-word matcher; no engine
        // change).
        let mut seed_query = String::from(query);
        if !chat_files.is_empty() {
            let chat_set: HashSet<&str> = chat_files.iter().map(|s| s.as_str()).collect();
            for f in &self.files {
                if chat_set.contains(f.path.as_str()) {
                    for d in &f.defs {
                        seed_query.push(' ');
                        seed_query.push_str(&d.name);
                    }
                }
            }
        }

        let seeds = graph.seed_nodes(&seed_query);
        if seeds.is_empty() {
            return Vec::new();
        }
        let mass = graph.personalized_pagerank(&seeds);
        let ranked = graph.ranked_memories(&mass); // Vec<(file_path, score)>

        let path_to_idx: HashMap<&str, usize> = self
            .files
            .iter()
            .enumerate()
            .map(|(i, f)| (f.path.as_str(), i))
            .collect();

        ranked
            .into_iter()
            .take(max_files)
            .filter_map(|(path, score)| {
                let fi = *path_to_idx.get(path.as_str())?;
                let f = &self.files[fi];
                let mut symbols = f.defs.clone();
                // Most "important" defs first: prefer type-level symbols, then
                // by source order.
                symbols.sort_by_key(|s| (kind_rank(s.kind), s.line));
                symbols.truncate(DEFAULT_SYMBOLS_PER_FILE);
                Some(RankedFile {
                    path,
                    score,
                    symbols,
                })
            })
            .collect()
    }

    /// Render an Aider-style text map: one section per ranked file with its top
    /// symbol signatures. Suitable for injection into an agent prompt.
    pub fn render_map(
        &self,
        query: &str,
        chat_files: &[String],
        max_files: usize,
        symbols_per_file: usize,
    ) -> String {
        let ranked = self.rank(query, chat_files, max_files);
        if ranked.is_empty() {
            return String::new();
        }
        let mut out = String::new();
        for rf in &ranked {
            out.push_str(&rf.path);
            out.push_str(":\n");
            for s in rf.symbols.iter().take(symbols_per_file) {
                out.push_str("  ");
                out.push_str(s.kind.as_str());
                out.push(' ');
                if s.signature.is_empty() {
                    out.push_str(&s.name);
                } else {
                    out.push_str(&s.signature);
                }
                out.push('\n');
            }
            out.push('\n');
        }
        out
    }
}

/// The default reader for [`CodeMap::build`] (v1.81, S1).
///
/// Opens `path` without following a final symlink and decides from the
/// **opened descriptor**, not a second path lookup: it must be a regular file,
/// single-link on Unix (a hard link can alias a file outside the tree), not a
/// reparse point on Windows, and no larger than `max_bytes`. The content is
/// read from that same descriptor. `None` on any refusal or non-UTF-8 data.
fn read_source_nofollow(path: &Path, max_bytes: usize) -> Option<String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK: a FIFO planted under the root must not hang the open;
        // it is then refused as a non-regular file.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // FILE_FLAG_OPEN_REPARSE_POINT: open the link itself, never its target.
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() > max_bytes as u64 {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return None;
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT
        if meta.file_attributes() & 0x400 != 0 {
            return None;
        }
    }
    let mut buf = Vec::new();
    file.take(max_bytes as u64 + 1).read_to_end(&mut buf).ok()?;
    if buf.len() > max_bytes {
        return None;
    }
    String::from_utf8(buf).ok()
}

/// Ordering key so structs/traits/classes surface above free functions when we
/// truncate a file's symbol list.
fn kind_rank(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Module => 0,
        SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Trait
        | SymbolKind::Class
        | SymbolKind::Interface
        | SymbolKind::TypeAlias => 1,
        SymbolKind::Function | SymbolKind::Method | SymbolKind::Macro | SymbolKind::Constant => 2,
    }
}

/// Parse one file and extract its definitions + identifier bag.
fn extract_file(root: &Path, path: &Path, lang: Lang, source: &str) -> Option<FileSymbols> {
    let mut parser = Parser::new();
    if parser.set_language(&lang.language()).is_err() {
        return None;
    }
    let tree = parser.parse(source, None)?;
    let root_node = tree.root_node();

    let rel = path
        .strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/");

    let mut defs: Vec<SymbolInfo> = Vec::new();
    let mut idents: HashSet<String> = HashSet::new();

    // Explicit stack walk (avoids deep recursion on large trees).
    let mut stack: Vec<Node> = vec![root_node];
    while let Some(node) = stack.pop() {
        let kind = node.kind();

        // Reference bag: collect identifier-ish leaf tokens.
        if is_identifier_kind(kind) {
            if let Some(text) = source.get(node.byte_range()) {
                if !text.is_empty() {
                    idents.insert(text.to_string());
                }
            }
        }

        // Definition?
        if let Some(sym_kind) = lang.def_kind(kind) {
            if let Some(name_node) = node.child_by_field_name("name") {
                if let Some(name) = source.get(name_node.byte_range()) {
                    let name = name.trim();
                    if !name.is_empty() {
                        defs.push(SymbolInfo {
                            name: name.to_string(),
                            kind: sym_kind,
                            line: node.start_position().row + 1,
                            signature: signature_of(source, &node),
                        });
                    }
                }
            }
        }

        // Push children.
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            stack.push(child);
        }
    }

    Some(FileSymbols { path: rel, defs, idents })
}

/// Node kinds we treat as identifier tokens for the reference bag.
fn is_identifier_kind(kind: &str) -> bool {
    matches!(
        kind,
        "identifier"
            | "type_identifier"
            | "field_identifier"
            | "scoped_type_identifier"
            | "property_identifier"
            | "shorthand_property_identifier"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, content: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(p, content).unwrap();
    }

    #[test]
    fn extracts_rust_symbols() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "lib.rs",
            "pub struct Widget { x: i32 }\npub fn build_widget() -> Widget { Widget { x: 1 } }\n",
        );
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        assert!(map.symbol_count() >= 2, "struct + fn indexed");
        let ranked = map.rank("build_widget", &[], 5);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].path, "lib.rs");
        assert!(ranked[0].symbols.iter().any(|s| s.name == "Widget"));
    }

    #[test]
    fn cross_file_reference_links_files() {
        let dir = tempdir().unwrap();
        // core.rs defines `compute`; user.rs references it.
        write(dir.path(), "core.rs", "pub fn compute() -> i32 { 42 }\n");
        write(
            dir.path(),
            "user.rs",
            "fn caller() -> i32 { compute() + compute() }\n",
        );
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        // Query mentions `compute` (defined in core.rs); both files should
        // surface, with the definer ranked at or above the referencer.
        let ranked = map.rank("where is compute defined", &[], 10);
        assert!(!ranked.is_empty());
        let paths: Vec<&str> = ranked.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"core.rs"), "definer present: {paths:?}");
    }

    #[test]
    fn empty_query_match_yields_empty() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "fn foo() {}\n");
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        assert!(map.rank("nonexistent_symbol_xyz", &[], 5).is_empty());
    }

    #[test]
    fn python_and_typescript_parse() {
        let dir = tempdir().unwrap();
        write(dir.path(), "svc.py", "def handler(req):\n    return 1\nclass Router:\n    pass\n");
        write(dir.path(), "app.ts", "export function render() { return handler; }\ninterface Props {}\n");
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        assert!(map.symbol_count() >= 3, "py def+class + ts fn(+interface)");
        let ranked = map.rank("handler Router render", &[], 10);
        assert!(!ranked.is_empty());
    }

    #[test]
    fn nonexistent_root_errors() {
        let cfg = CodeMapConfig::new("/nonexistent/path/xyzzy/12345");
        assert!(CodeMap::build(&cfg).is_err());
    }

    /// Live verification against real repository source: build over this
    /// crate's own `src/` and confirm a query for `TripleGraph` surfaces
    /// `graph_rank.rs` (the file that actually defines it).
    #[test]
    fn live_ranks_real_source_tree() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let map = CodeMap::build(&CodeMapConfig::new(&src)).unwrap();
        assert!(map.file_count() >= 5, "should index multiple real .rs files");
        assert!(map.symbol_count() >= 50, "real crate has many symbols");
        let ranked = map.rank("TripleGraph personalized_pagerank", &[], 5);
        assert!(!ranked.is_empty(), "query should match real symbols");
        assert_eq!(
            ranked[0].path, "graph_rank.rs",
            "graph_rank.rs defines TripleGraph, must rank first"
        );
    }

    /// v1.81 (S1): `build()` never reads through a symlink (file or
    /// directory) or a multiply-linked file.
    #[cfg(unix)]
    #[test]
    fn build_skips_symlinks_and_hard_links() {
        let dir = tempdir().unwrap();
        let outside = tempdir().unwrap();
        write(outside.path(), "leak.rs", "pub fn leaked_by_link() {}\n");
        write(outside.path(), "pkg/mod.rs", "pub fn leaked_by_dir() {}\n");
        write(outside.path(), "hard.rs", "pub fn leaked_by_hard() {}\n");
        write(dir.path(), "ok.rs", "pub fn fine() {}\n");
        std::os::unix::fs::symlink(outside.path().join("leak.rs"), dir.path().join("a.rs")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("pkg"), dir.path().join("pkg")).unwrap();
        let hard = fs::hard_link(outside.path().join("hard.rs"), dir.path().join("hard.rs")).is_ok();
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        assert_eq!(map.symbol_count(), 1, "only ok.rs: hard link made = {hard}");
        assert!(map.rank("leaked_by_link leaked_by_dir leaked_by_hard", &[], 5).is_empty());
    }

    /// `build_with_reader` indexes exactly what the reader returns, hands it
    /// root-relative paths, and drops over-cap content.
    #[test]
    fn build_with_reader_is_the_read_boundary() {
        let dir = tempdir().unwrap();
        write(dir.path(), "a.rs", "pub fn alpha() {}\n");
        write(dir.path(), "sub/b.rs", "pub fn beta() {}\n");
        let mut seen: Vec<String> = Vec::new();
        let map = CodeMap::build_with_reader(&CodeMapConfig::new(dir.path()), |_walked, rel, _max| {
            let rel = rel.to_string_lossy().replace('\\', "/");
            seen.push(rel.clone());
            (rel == "sub/b.rs").then(|| "pub fn beta() {}\n".to_string())
        })
        .unwrap();
        seen.sort();
        assert_eq!(seen, vec!["a.rs".to_string(), "sub/b.rs".to_string()]);
        assert_eq!(map.symbol_count(), 1);
        assert!(map.rank("alpha", &[], 5).is_empty());

        let mut cfg = CodeMapConfig::new(dir.path());
        cfg.max_file_bytes = 4;
        let map = CodeMap::build_with_reader(&cfg, |_, _, _| Some("pub fn too_long() {}".into())).unwrap();
        assert_eq!(map.symbol_count(), 0, "over-cap content is dropped");
    }

    #[test]
    fn chat_files_seed_ranking() {
        let dir = tempdir().unwrap();
        write(dir.path(), "core.rs", "pub fn alpha() {}\npub fn beta() {}\n");
        write(dir.path(), "other.rs", "fn uses_alpha() { alpha(); }\n");
        let map = CodeMap::build(&CodeMapConfig::new(dir.path())).unwrap();
        // Empty query but core.rs is "in the chat" — its symbols seed the walk.
        let ranked = map.rank("", &["core.rs".to_string()], 10);
        assert!(!ranked.is_empty(), "chat-file symbols should seed");
    }
}
