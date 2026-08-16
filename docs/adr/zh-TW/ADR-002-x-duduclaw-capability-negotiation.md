# ADR-002 — x-duduclaw Header 版本控制與能力協商

**Status:** Implemented (2026-05-06)
**Sprint:** W22-P0
**Owner:** TL-DuDuClaw
**Implemented in:** `crates/duduclaw-cli/src/mcp_headers.rs` + `mcp_capability.rs`
**Last updated:** 2026-05-06

---

## Context

DuDuClaw 對外暴露一個 HTTP/SSE MCP endpoint(`/mcp/v1/call`、`/mcp/v1/stream`),供外部
client 使用(Claude Desktop、CI pipeline、第三方整合)。隨著平台推出新能力(A2A Bridge、
Secret Manager、簽章 agent card)並引入破壞性變更,client 需要一個可靠、機器可讀的方式來:

1. **發現**目前對話的伺服器上有哪些能力可用。
2. **宣告**自己需要哪些能力,讓伺服器能提早拒絕不相容的請求(在浪費 token 或做半套工作
   之前)。
3. **理解**伺服器的 HTTP API 相容等級,與 DuDuClaw 的 SemVer 發版號脫鉤。

沒有正式的 header 協定,能力發現會退化成文件漂移與難以在 client/server 版本落差之間診斷的
執行期意外。

---

## Decision

引入三個 HTTP response header 與一個 HTTP request header,統稱為
**x-duduclaw header protocol**,套用在 HTTP 伺服器上的每一筆請求/回應。

### §1 — Header 定義

| Header | Direction | Description |
|--------|-----------|-------------|
| `x-duduclaw-version` | Response | HTTP API 相容版本(見 §4.3) |
| `x-duduclaw-capabilities` | Response | 逗號分隔的已啟用伺服器能力清單(見 §4.1) |
| `x-duduclaw-capabilities` | Request(optional) | client 宣告的本次請求所需能力 |
| `x-duduclaw-missing-capabilities` | Response(僅 422) | 伺服器無法滿足的請求能力子集 |

### §2 — 不變式:每個回應都要帶 Header

`x-duduclaw-version` 與 `x-duduclaw-capabilities` 必須出現在**每一個** HTTP 回應上,
包括錯誤回應(4xx、5xx)。這由 `inject_capability_headers` 這個 Axum middleware 強制執行,
它包住所有路由,包含 `negotiate_capabilities` 產生的 422。

### §3 — 能力協商協定

#### §3.1 — Client 端(request)
Client **可以**在請求中帶上 `x-duduclaw-capabilities` 來宣告自己需要哪些能力:

```
x-duduclaw-capabilities: memory/3,mcp/2
```

#### §3.2 — 寬容模式(Permissive Mode)
如果 client 省略這個 header(或送出空值/格式錯誤的值),伺服器會把這個請求當作沒有能力
需求並放行。**缺席 = 寬容,不是拒絕。**

#### §3.3 — 422 Unprocessable Entity
如果 client 帶了這個 header,而其中任何一項需求無法被滿足:

| Failure mode | Server response |
|---|---|
| 能力不存在或 `enabled: false` | 422 — `server_version: null` |
| 能力存在但主版本號太低 | 422 — `server_version: <current_v>` |

422 body:
```json
{
  "error": "capability_mismatch",
  "message": "Required capabilities not available on this server",
  "missing": [
    { "capability": "a2a", "required_version": 1, "server_version": null },
    { "capability": "mcp", "required_version": 5, "server_version": 2 }
  ]
}
```

422 回應同時帶有 `x-duduclaw-missing-capabilities: a2a/1,mcp/5`,方便機器解析而不需要
反序列化 body。

### §4 — 格式規格

#### §4.1 — x-duduclaw-capabilities 格式
```
memory/<major>,<other-cap-alpha>/<major>,...
```

規則:
1. 只有 `CAPABILITY_REGISTRY` 裡 `enabled: true` 的項目會出現。
2. `memory` **永遠排第一個**:它是 DuDuClaw 核心的差異化能力。
3. 其他所有已啟用的能力依**字典序(ASCII)**排列在後面。
4. 每個項目是 `<name>/<major_version>`,不含空格。
5. 項目之間以逗號分隔,不含空格。

範例:`memory/3,audit/2,governance/1,mcp/2,skill/1,wiki/1`

#### §4.2 — 主版本號語意
capability registry 裡的 `major_version` **只在該能力發生破壞性協定變更時**才會遞增。
在某能力內新增選填欄位或新工具不算主版本躍升。釘選 `mcp/2` 的 client 可以信賴所有
`mcp/2.x` 行為維持穩定。

#### §4.3 — x-duduclaw-version 語意
這個版本追蹤的是 HTTP API 相容性,與 DuDuClaw 的 SemVer 發版(`v1.11.x` 等)脫鉤。
只有在 HTTP API 本身發生相容性變更時(新增必要 header、status code 語意改變等)才會
變動。

目前值:`1.2`
- `1`:HTTP API 穩定(過了 beta 期,W20 引入)
- `2`:第二次向下相容的 HTTP 變更(W22,即本 ADR,加入能力協商)

### §5 — 能力登記表(Capability Registry)

正式的 registry 位於 `crates/duduclaw-cli/src/mcp_headers.rs::CAPABILITY_REGISTRY`。

| Capability | Major Version | Status | Sprint |
|---|---|---|---|
| `memory` | 3 | ✅ Enabled | core |
| `audit` | 2 | ✅ Enabled | W20-P1 |
| `governance` | 1 | ✅ Enabled | W19-P1 |
| `mcp` | 2 | ✅ Enabled | W20 HTTP/SSE Phase 2 |
| `skill` | 1 | ✅ Enabled | — |
| `wiki` | 1 | ✅ Enabled | — |
| `a2a` | 1 | 🔒 Disabled | W21(pending enablement) |
| `secret-manager` | 1 | 🔒 Disabled | W22 P0(pending) |
| `signed-card` | 1 | 🔒 Disabled | W22 P1(pending) |

已停用的能力**絕不會**出現在外送的 header 裡。當 client 要求一個已停用的能力,伺服器回
422,`server_version: null`。

### §6 — Axum Middleware Layer 順序

```
router
    .layer(middleware::from_fn(negotiate_capabilities))    // INNER(請求時最先檢查)
    .layer(middleware::from_fn(inject_capability_headers)) // OUTER(所有回應都會加上 header)
```

Axum 以反向註冊順序評估 layer(最後一個 `.layer()` 是最外層)。這個順序保證
`inject_capability_headers` 會在**所有**回應上執行,包括內層 `negotiate_capabilities`
middleware 產生的 422。

### §7 — 停用能力的政策

`enabled: false` 的能力,從 client 的角度看,行為與未知能力完全相同:422 body 裡
`server_version: null`。這避免透過 header 協定洩漏實作路線圖資訊。

已停用的能力會從外送的 `x-duduclaw-capabilities` header 中省略,所以機會式發現
(client 讀取回應 header 並自行調整)也能正常運作。

### §8 — SDK 與文件同步要求

當能力登記表發生變更(新增能力、啟用能力、或主版本躍升)時:

1. 更新 `mcp_headers.rs` 裡的 `CAPABILITY_REGISTRY`
2. 更新 snapshot test `header_snapshot_matches_expected`。這是能力變更在悄悄上線前的
   強制暫停點
3. 更新本 ADR 的 §5 表格
4. 更新 CHANGELOG.md
5. 若新能力需要 client 端的 feature flag,通知 SDK 維護者

---

## Consequences

### Positive(正向)

- **零成本發現**:每個回應本來就帶著能力 metadata,不需要額外的往返。
- **明確契約**:宣告需求的 client 會得到 422 + 診斷資訊,不會是無聲的部分失敗。
- **預設可加**:沒有送 request header 的 client 不會被新上線的能力破壞。
- **測試鎖定的 registry**:snapshot test 防止 registry 的意外變更悄悄流進 production。

### Negative / Trade-offs(取捨)

- **每請求的額外開銷**:`build_capabilities_header()` 在每個回應上都會遍歷
  `CAPABILITY_REGISTRY`(目前 9 個項目)。在現有規模下可接受;若 registry 成長超過
  100 個項目,可考慮預先計算成靜態的 `OnceLock<String>`。
- **只協商主版本**:client 無法表達細粒度的 minor/patch 需求。這是刻意的設計
  (minor/patch 變更依定義永遠向下相容)。

---

## Implementation

| File | Role |
|------|------|
| `crates/duduclaw-cli/src/mcp_headers.rs` | Registry、header builder、parser、協商邏輯(23 個 unit test) |
| `crates/duduclaw-cli/src/mcp_capability.rs` | Axum middleware(`inject_capability_headers`、`negotiate_capabilities`),11 個整合測試 |
| `crates/duduclaw-cli/src/mcp_http_server.rs` | 把兩個 middleware layer 接進 `build_router()` |
| `crates/duduclaw-cli/src/lib.rs` | 匯出 `pub mod mcp_headers` + `pub mod mcp_capability` |

**Test coverage:** 34 個測試(unit + 透過 `tower::ServiceExt::oneshot` 做的 Axum 整合測試),
全數通過。

---

## Alternatives Considered

### Alt A — 只用版本化 URL 路徑(`/mcp/v2/call`)
否決:URL 版本控制能處理主要 API 修訂,但無法表達細粒度的能力存在與否。連上
`/mcp/v2/call` 的 client 仍然不知道這個特定部署上有沒有 `a2a` 或 `secret-manager`。

### Alt B — 能力發現端點(`GET /mcp/capabilities`)
否決:每個 session 開始前都要多一次往返。基於 header 的發現是零成本的,因為資訊已經
搭便車在每一個既有回應上。

### Alt C — 把能力放進 JSON-RPC `initialize` 結果裡
否決:HTTP layer 位於 JSON-RPC layer 之下。有些路由(例如 `/healthz`)根本不會處理
JSON-RPC envelope。Header 才是 HTTP 層級協定協商該在的正確層級。

---

*ADR written by TL-DuDuClaw | 2026-05-06*
