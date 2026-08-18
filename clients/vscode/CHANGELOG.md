# Changelog

本檔記錄 DuDuClaw VS Code 擴充功能的重要變更。格式依循
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/)。

## [Unreleased]

## [0.3.0] - 2026-08-18

對應 `commercial/docs/DESIGN-vscode-client-v2-2026-08.md` §4 P1「把 IDE 變成第 12 個通道做好」。

### Added

- 編輯器上下文：右鍵選單／指令 `DuDuClaw: 把選取內容問 AI 員工`，取得選取內容＋相對路徑＋行號範圍，包成 DATA 圍欄區塊附進對話；聊天紀錄中以可摺疊 `<details>` 區塊顯示，選取內容上限 8KB（超過會截斷並標註）。
- 輸入模式切換（問一問／交辦／想一想）：交辦與想一想改走 `tasks.goal_create` RPC（`plan_first`），對應 dashboard AssignSheet 的第二、三種執行模式；需要對該 AI 員工有 Operator 級以上權限，權限不足時沿用既有的「此功能需要管理者權限。」友善訊息。
- 任務分頁（第三個分頁，所有角色可見）：列出目前選中 AI 員工的任務（`tasks.list`），狀態與「等你決定」原因（H11 pause reason）皆有標籤；`needs_human` 卡片可直接重試（可附下一輪指示）／標記完成／放棄（`tasks.goal_decide`，與 dashboard 的 `NeedsHumanActions` 同一 RPC 形狀）。切到此分頁時自動載入一次，另有手動重新整理按鈕，不做輪詢。
- 對話 session resume：WebChat 協定的 `session_info`／`user_message.session_id` 支援客戶端指定續聊；重新連線（例如 VS Code 重啟）後會嘗試依 (gatewayUrl, agent) 找回上次的 session 並接續對話，找不到或已失效時安靜地退回全新對話，不會卡在錯誤迴圈。

### Changed

- package.json：`description` 更新以反映交辦／任務功能；新增 `keywords`（delegate / goal loop / task）。

## [0.2.0] - 2026-08-17

對應設計文件 §4 P0「多 Agent＋角色感知」。

### Added

- 多 AI 員工選擇器：面板頂部下拉選單 + `DuDuClaw: 切換 AI 員工` 指令，資料源 `agents.list`（伺服器已依綁定過濾）；每位員工獨立 session，記住 workspace 最後選擇。
- 角色感知 UI：登入後呼叫 `GET /api/me`，員工 (employee) 角色自動隱藏審批分頁，不再讓使用者點了才看到「permission denied」。
- 狀態列：顯示目前 AI 員工與帳號角色，點擊可切換。
- 存取權限徽章：下拉選單旁顯示對該 AI 員工的權限等級（完整權限／可操作／僅檢視）。

## [0.1.0]

### Added

- 初版：對話與審批兩個分頁、`DuDuClaw: 登入 Gateway` / `DuDuClaw: 登出` 指令、可設定的 `duduclaw.gatewayUrl`。JWT 存 VS Code Secret Storage，所有連線都在 extension host 端（webview 純 UI，不含任何 token）。
