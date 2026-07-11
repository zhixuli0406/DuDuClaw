# ADR-006: Local OCR for sensitive images

- Status: Accepted (方向定案;OCR 引擎選型待實測)
- Date: 2026-07-09
- Deciders: DuDuClaw maintainers

## Context

客戶在 demo 提出(§7,23:30-24:58):機敏圖片(含身分證號、合約、單據)
若走雲端 OCR,資料會離開地端,有外流風險。要求地端 OCR;地端若不穩,寧可
直接封鎖圖片上傳,也不要把機敏圖片送出去。

現況錨點:**專案沒有任何 OCR。** grep 整個 crates 找不到 tesseract / mineru /
`ocr` 的實作。`allow_image_input` 這個 capability 旗標也還沒建。deny-by-default
的能力機制已存在(`CapabilitiesConfig`,`crates/duduclaw-core/src/types.rs:471`),
可以掛新旗標。`TODO-feature-gaps` §八 已規劃 MinerU 做 PDF(Python bridge)。

這是一個選型 spike。麻煩在於「哪個 OCR 引擎的繁體中文辨識率夠好」不是查
規格表能回答的問題——它取決於真實樣張。

## Options considered

**(a) MinerU**
規劃中(已在 §八 為 PDF 排程)、版面感知(能處理表格 / 多欄 / 圖文混排)、
品質上限高。缺點是重——Python 依賴多、模型大、啟動與記憶體成本高。

**(b) Tesseract subprocess**
輕、成熟、跨平台、裝起來便宜。缺點是**繁體中文品質待驗**——Tesseract 的
中文模型對印刷體尚可,對表格、手寫、低解析度、直排的表現是未知數,不能
憑訓練資料瞎猜它「應該可以」。

**(c) macOS Vision framework**
Apple 原生、繁中辨識實測口碑好、零額外依賴。硬傷是平台限定——只能在
macOS 跑,gateway 部署到 Linux 就沒了。

## Decision

**不預先選定 OCR 引擎。** 先跑一次真實的辨識率測試——**10 張繁體中文樣張**
(涵蓋身分證 / 單據 / 合約 / 表格等實際場景),量三家的辨識率,拿數字拍板。
專案守則是「不要瞎猜 / 先量再選」,OCR 品質這種東西沒有實測就沒有結論。

在選型定案前,先量什麼:
- 逐字辨識率(尤其數字與身分證號這類 redaction pipeline 要抓的欄位)。
- 版面破壞程度(表格 / 多欄有沒有被打亂到後續 redaction 抓不到)。
- 每張延遲與資源占用(地端要能撐)。

**無論 OCR 選哪家,block-first fallback 照樣先出。** WP12-T12.1 的封鎖 gate
與 OCR 引擎無關,而且便宜又 fail-closed:`agent.toml [capabilities]
allow_image_input`(預設 true 向後相容),設 false 時各 channel 收到圖片就
不下載、不進 context,回一則 zh-TW 說明。九個 channel 下載媒體的 choke-point
各不相同,逐一接、逐項勾。這條先交付,把「機敏圖片外流」這個最尖銳的風險
用最保守的方式堵住,不等 OCR 選型。

OCR 路徑本身(T12.3)在引擎選定後才接:圖片 → 地端 OCR → 文字走既有
redaction pipeline(WP2 規則直接生效)→ 才進 LLM context。OCR 失敗且
`allow_image_input=false` ⇒ 封鎖;OCR 失敗但 allow ⇒ config 二選一
(`degrade: block | passthrough`,預設 block,fail-closed)。

## Consequences

**得到的:** 選型基於真實數字而非規格表想像;block-first 這條 fail-closed 的
便宜路徑先落地,機敏圖片外流風險立刻收斂,不被 OCR 選型卡住。

**付出的:** OCR 完整功能要等 10 張樣張的實測跑完才動工,這是刻意的延遲——
寧可晚一點選對,不要早一點選錯。三家各有致命弱點(MinerU 重、Tesseract 繁中
未驗、Vision 平台鎖),沒有一個能不看數字就選。

**交付分級:** 依 WP12 DoD,OCR 品質未達標時,T12.1 封鎖先行交付即可標
PARTIAL。block-first 是可以獨立交付並驗收的最小單位。

**選定之後:** OCR 引擎一旦依實測數字拍板,以本 ADR 的補記或新 ADR 記錄
選了哪家、依據哪些數字。在那之前,本 ADR 的 OCR 引擎欄位保持「待實測」,
不填任何未經量測的傾向。
