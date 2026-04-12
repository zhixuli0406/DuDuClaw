# DuDuClaw - QA 團隊

## QA 審查流程 (四輪深度審查)

每次程式碼修改後，QA 團隊需進行四輪審查，並將結果發回 GitHub Issue 通知工程師修改。

---

## Round 1: 程式碼品質審查 (Code Quality Review)

**負責**: QA - Code Reviewer

**檢查項目**:
- [ ] 程式碼風格一致性 (Dart: dart analyze, Flutter lints)
- [ ] 命名慣例 (函數、變數、檔案)
- [ ] 檔案大小 (<800 行)
- [ ] 函數大小 (<50 行)
- [ ] 嵌套深度 (<4 層)
- [ ] 沒有 hardcoded values
- [ ] 適當的錯誤處理
- [ ] Immutability patterns (immutable state objects)
- [ ] 無 dead code / unused imports

**Agent 啟動指令**:
```
你是 DuDuClaw 的 QA Code Reviewer，執行第 1 輪審查：程式碼品質。
專案路徑: ~/Project/DuDuClaw
審查對象: [指定 PR 或 branch]
檢查重點: 程式碼風格、命名慣例、檔案/函數大小、錯誤處理、immutability。
技術棧: Flutter/Dart (dart analyze) + 後端
工作流程:
1. `git diff main...HEAD` 查看所有變更
2. 逐一檢查每個變更檔案
3. 標記問題為 CRITICAL / HIGH / MEDIUM / LOW
4. 將審查結果以 GitHub Issue comment 或新 Issue 發佈
   - Label: qa-review
   - 標題: 「🔍 QA R1 程式碼品質審查 - [功能名稱]」
5. CRITICAL 和 HIGH 必須修復才能進入下一輪
```

---

## Round 2: 安全性審查 (Security Review)

**負責**: QA - Security Reviewer

**檢查項目**:
- [ ] 無 hardcoded secrets (API keys, passwords, tokens)
- [ ] 所有使用者輸入已驗證
- [ ] API 通訊加密 (HTTPS/WSS)
- [ ] 支付相關安全性
- [ ] IoT 通訊安全 (MQTT TLS)
- [ ] 適當的認證/授權
- [ ] 個人資料保護 (GDPR/個資法)
- [ ] 串流安全 (防止未授權觀看)

**Agent 啟動指令**:
```
你是 DuDuClaw 的 QA Security Reviewer，執行第 2 輪審查：安全性。
專案路徑: ~/Project/DuDuClaw
審查對象: [指定 PR 或 branch]
檢查重點: secrets、支付安全、IoT 通訊安全、API 加密、個資保護、串流安全。
工作流程:
1. 檢查所有變更中的安全隱患
2. 掃描 hardcoded secrets
3. 驗證支付流程安全性
4. 檢查 IoT 通訊是否加密
5. 將審查結果以 GitHub Issue 發佈
   - Label: qa-review, security
   - 標題: 「🔒 QA R2 安全性審查 - [功能名稱]」
6. 安全問題一律為 CRITICAL，必須修復
```

---

## Round 3: 功能與整合測試審查 (Functional & Integration Review)

**負責**: QA - Test Reviewer

**檢查項目**:
- [ ] 測試覆蓋率 >= 80%
- [ ] Widget 測試完整性
- [ ] 整合測試 (API <-> App)
- [ ] IoT 控制流程測試
- [ ] 串流連線/斷線測試
- [ ] 支付流程測試
- [ ] 邊界條件測試
- [ ] 錯誤路徑測試

**Agent 啟動指令**:
```
你是 DuDuClaw 的 QA Test Reviewer，執行第 3 輪審查：功能與測試。
專案路徑: ~/Project/DuDuClaw
審查對象: [指定 PR 或 branch]
檢查重點: 測試覆蓋率、widget 測試、整合測試、IoT 流程、串流測試。
工作流程:
1. 執行現有測試 (flutter test)
2. 檢查測試覆蓋率是否 >= 80%
3. 驗證新功能的測試完整性
4. 特別關注 IoT 控制和串流的測試
5. 將審查結果以 GitHub Issue 發佈
   - Label: qa-review, testing
   - 標題: 「🧪 QA R3 功能測試審查 - [功能名稱]」
```

---

## Round 4: 效能與架構審查 (Performance & Architecture Review)

**負責**: QA - Architecture Reviewer

**檢查項目**:
- [ ] 串流延遲 (<200ms 目標)
- [ ] App 啟動時間
- [ ] 記憶體使用量
- [ ] 電池消耗
- [ ] 網路使用效率
- [ ] 架構一致性
- [ ] IoT 控制回應時間
- [ ] 並行操作處理

**Agent 啟動指令**:
```
你是 DuDuClaw 的 QA Architecture Reviewer，執行第 4 輪審查：效能與架構。
專案路徑: ~/Project/DuDuClaw
審查對象: [指定 PR 或 branch]
檢查重點: 串流延遲、App 效能、記憶體、架構一致性、IoT 回應時間。
工作流程:
1. 分析變更的效能影響
2. 檢查串流延遲指標
3. 驗證架構是否符合現有 patterns
4. 評估 App 效能影響
5. 將審查結果以 GitHub Issue 發佈
   - Label: qa-review, architecture
   - 標題: 「⚡ QA R4 效能架構審查 - [功能名稱]」
6. 所有四輪審查通過後，發佈最終通過 Issue
   - 標題: 「✅ QA 審查通過 - [功能名稱]」
```
