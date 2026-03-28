# Code Review: Model Registry + Per-Agent Routing + Onboard

> 3-Agent 深度 Code Review 報告
> Date: 2026-03-28
> Scope: `model_registry/` (4 modules), `claude_runner.rs` routing, `main.rs` onboard, `mcp.rs` handlers
> Reviewers: Security / Code Quality / Architecture

---

## Executive Summary

| 嚴重度 | Security | Code Quality | Architecture | 合計 |
|--------|----------|-------------|-------------|------|
| **CRITICAL** | 2 | 1 | — | **3** |
| **HIGH** | 4 | 5 | — | **9** |
| **P0** | — | — | 1 | **1** |
| **P1** | — | — | 4 | **4** |
| **MEDIUM** | 5 | 5 | — | **10** |
| **LOW** | 3 | 3 | — | **6** |
| **P2** | — | — | 6 | **6** |

**結論: BLOCK MERGE** — 3 CRITICAL + 1 P0 必須修復。

---

## CRITICAL / P0 Issues

### C-1. Path Traversal in downloader — filename not validated at entry
- **Source**: Security
- **File**: `downloader.rs:61,90-91` + `hf_api.rs:117`
- **Issue**: `download_model()` accepts any `filename` without validation. Onboard flow passes `entry.filename` from HF API directly, bypassing MCP layer checks. Malicious HF repo can return `../../.ssh/authorized_keys.gguf`.
- **Fix**: Validate in `download_model()` entry: `^[A-Za-z0-9_.-]+\.gguf$`. Also filter in `hf_api::convert_hf_models`.

### C-2. SSRF via unvalidated `repo` parameter
- **Source**: Security + Code Quality (independently found)
- **File**: `mcp.rs:1888-1889`
- **Issue**: `repo` only checked for empty, directly interpolated into URL. Can construct arbitrary request targets.
- **Fix**: Validate `repo` matches `^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$`.

### C-3 / P0. `inference_mode="local"` + `prefer_local=false` = dead end
- **Source**: Architecture
- **File**: `claude_runner.rs:63-92`
- **Issue**: When global mode is "local" but agent's `prefer_local=false`, neither local nor Claude path executes → agent cannot infer at all. Also: `inference_mode="claude"` doesn't prevent local inference if `prefer_local=true`.
- **Fix**: Add global mode gate BEFORE per-agent routing:
  ```
  "local"  → force local (ignore prefer_local)
  "claude" → skip local entirely
  "hybrid" → respect per-agent prefer_local
  ```

---

## HIGH / P1 Issues

### Security (4)
| ID | File | Issue |
|----|------|-------|
| H-S1 | `downloader.rs:107-109` | **HF token sent to mirror** — `hf-mirror.com` receives `HF_TOKEN` bearer auth |
| H-S2 | `curated.rs:28` | **is_trusted case-insensitive** — `eq_ignore_ascii_case` allows org name confusion |
| H-S3 | `hf_api.rs:250-251` | **Cache poisoning** — cache file readable/writable, inject malicious filenames |
| H-S4 | `mcp.rs:1899` | **HF token leak in error** — `{e}` may contain request headers |

### Code Quality (5)
| ID | File | Issue |
|----|------|-------|
| H-Q1 | `main.rs:929` | **`prefer_local` always true** — dead if-else, hybrid mode always prefers local |
| H-Q2 | `hf_api.rs:64,102` | **Cache key mismatch** — saves `query` but searches `search_query` (with "gguf" appended) |
| H-Q3 | `hf_api.rs:229` | **estimate_min_ram underestimates** — 1.2x overhead, should be 1.5x |
| H-Q4 | `hf_api.rs:237` | **Tier sort uses raw `as u8` cast** — fragile, depends on implicit discriminant |
| H-Q5 | `main.rs:343-345` | **HF API blocks onboard** — 10s timeout with no user feedback |

### Architecture P1 (4)
| ID | File | Issue |
|----|------|-------|
| P1-1 | `claude_runner.rs:63-79` | **"claude" mode doesn't block local** — per-agent `prefer_local` overrides global mode |
| P1-2 | `claude_runner.rs:160`, `types.rs:64` | **`use_router` per-agent config ignored** — reads global router config instead |
| P1-3 | `claude_runner.rs:96-105` | **`read_inference_mode()` reads disk every call** — hot path, no cache |
| P1-4 | `mcp.rs:1600-1808` | **MCP handlers rebuild InferenceEngine every call** — no singleton reuse |

---

## MEDIUM Issues (10)

<details>
<summary>展開</summary>

| ID | Source | File | Issue |
|----|--------|------|-------|
| M-1 | Security | `hf_api.rs:70-73` | Query length unlimited, can create oversized URLs |
| M-2 | Security | `downloader.rs:184` | Race condition on concurrent downloads (partial file) |
| M-3 | Security | `mod.rs:82-94` | `download_url()` doesn't URL-encode repo/filename |
| M-4 | Security | `main.rs:905` | Full URL shown on download failure |
| M-5 | Security | `hf_api.rs:283-285` | Cache write silently fails |
| M-6 | Quality | `mcp.rs:1888` | URL construction duplicated (not using `RegistryEntry` methods) |
| M-7 | Quality | `hf_api.rs`, `downloader.rs` | `reqwest::Client` rebuilt every call, no connection pooling |
| M-8 | Quality | `hf_api.rs:262` | `u64` subtraction can underflow on clock skew |
| M-9 | Quality | `hf_api.rs:167` | `pick_best_quantization` has redundant case variants |
| M-10 | Quality | `main.rs:298,318,929` | `inference_mode` is raw `usize`, should be enum |

</details>

---

## Priority Remediation Order

### Phase A: CRITICAL + P0 (must fix)
1. **C-1** downloader filename validation
2. **C-2** repo format validation
3. **C-3/P0** global mode gate in `call_claude_for_agent`

### Phase B: HIGH + P1 (before release)
4. **H-S1** don't send HF token to mirror
5. **H-S2** `is_trusted` exact match
6. **H-Q1** fix `prefer_local` dead code
7. **P1-1** "claude" mode blocks local inference
8. **P1-3** cache `read_inference_mode`
9. **H-Q2** fix cache key to use `search_query`
10. **H-Q3** `estimate_min_ram` → 1.5x
