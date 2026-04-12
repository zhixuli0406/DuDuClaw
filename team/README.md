# DuDuClaw - AI 團隊組織

## 團隊架構

```
                    ┌──────────────┐
                    │   Louis      │
                    │  (老闆/你)    │
                    └──────┬───────┘
                           │
              ┌────────────┼────────────┐
              │            │            │
    ┌─────────▼──────┐ ┌───▼───┐ ┌─────▼─────────┐
    │  Team Leader   │ │  PM   │ │    Agnes       │
    │  (排程 Agent)  │ │(排程)  │ │  (統一入口)    │
    │  每日 10:42    │ │每日9:03│ │               │
    └─────────┬──────┘ └───────┘ └───────────────┘
              │
    ┌─────────┼─────────┐
    │         │         │
┌───▼────┐ ┌──▼────┐ ┌──▼──────┐
│Flutter │ │Back-  │ │Stream- │
│Engineer│ │end/IoT│ │ing     │
│(隨需)  │ │Eng.   │ │Eng.    │
└───┬────┘ └──┬────┘ └──┬─────┘
    │         │         │
    └─────────┼─────────┘
              │
    ┌─────────▼────────┐
    │   QA 團隊        │
    │  (四輪審查)       │
    │  R1: 品質        │
    │  R2: 安全        │
    │  R3: 測試        │
    │  R4: 效能/架構    │
    └──────────────────┘
```

## 排程 Agent (RemoteTrigger)

| 角色 | Trigger ID | 排程 | 說明 |
|------|-----------|------|------|
| PM | `trig_01HE673hHVrWywVrkapwZkmc` | 每日 09:03 | 競品分析、技術研究、功能提案 |
| Team Leader | `trig_015hJiM78GqhoPHT7hTu7stm` | 每日 10:42 | 進度追蹤、工作分配（統一管理兩專案） |

## 隨需 Agent (透過 Agnes 啟動)

| 角色 | 專長 | 啟動方式 |
|------|------|---------|
| Flutter Engineer | Flutter, Dart, Mobile UI/UX | 告訴 Agnes 要啟動 Flutter 工程師 |
| Backend/IoT Engineer | 後端 API, IoT 硬體控制, MQTT | 告訴 Agnes 要啟動後端/IoT 工程師 |
| Streaming Engineer | WebRTC, 即時串流, 低延遲 | 告訴 Agnes 要啟動串流工程師 |
| QA R1 | 程式碼品質審查 | 告訴 Agnes 要做程式碼審查 |
| QA R2 | 安全性審查 | 告訴 Agnes 要做安全審查 |
| QA R3 | 功能測試審查 | 告訴 Agnes 要做測試審查 |
| QA R4 | 效能架構審查 | 告訴 Agnes 要做架構審查 |

## 工作流程

1. **PM** 每日自動搜尋產業資訊，發佈功能提案到 GitHub Issue
2. **Team Leader** 每日自動檢視 PM 提案 + 專案狀態，發佈進度報告
3. **你** 透過 Agnes 查看報告，決定要做什麼
4. Agnes 啟動對應的 **工程師** Agent 執行開發
5. 開發完成後，Agnes 依序啟動 **QA** 四輪審查
6. QA 發現問題 → 回報工程師 → 修改 → 重新審查
7. 全部通過 → 合併 PR

## 檔案索引

- `team/engineers.md` - 工程師團隊定義與 Agent 指令
- `team/qa.md` - QA 團隊定義與四輪審查流程
