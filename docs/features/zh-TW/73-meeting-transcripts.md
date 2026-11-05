# 會議逐字稿

[English](../73-meeting-transcripts.md) · [日本語](../ja-JP/73-meeting-transcripts.md)

將 Meet、Zoom、Teams 等平台下載的錄影或錄音，轉成臺灣繁體中文 Markdown 與 JSON，附上時間戳及可辨識的講者標籤。本功能處理已錄好的檔案，不會加入或錄製即時會議，也不會直接讀取 Google Drive；沒有新增 OAuth 權限。

## 大檔案上傳

開啟控制台「會議逐字稿」，選擇你有權使用的員工，再選擇錄音或影片。非管理員只能選擇自己綁定、可選取且具 Operator／Owner 權限的員工，不能使用主代理或需要管理員權限的機器員工。每段上傳為 8 MiB，伺服器逐段核對 SHA-256。中斷後選擇同一個檔案，可接續尚未完成的部分；未完成上傳若 2 小時未收到新分段即過期。

支援 WebCodecs 的瀏覽器會嘗試將 MP4／MOV 的 AAC-LC 音軌轉成單聲道、約 48 kbps 的 AAC，再上傳較小的音訊。WebM、其他音軌格式、瀏覽器不支援或轉換失敗時，上傳原檔；頁面會顯示採用哪一種方式。媒體解析使用固定版本的 mp4box.js（BSD-3-Clause）。這是盡力縮檔，不保證每部影片都能在瀏覽器轉換。

上傳完成後，工作列表會顯示排隊、準備音訊、正在轉錄的段落、完成、失敗或取消。完成後可下載 `.md` 與 `.json`。取消會阻止下一段開始；已送到服務商的那一段會先結束並記錄可取得的費用，不保證免除已發生的費用。

## 在對話中使用

`meeting_transcribe` 接受自己的附件路徑，立即回傳 `job_id` 與狀態，不需要讓 MCP 連線維持到轉錄結束。

```json
{"source_path":"meeting.mp4","language_hint":"zh-TW","write_wiki":false}
```

- `meeting_transcribe_status {"job_id":"…"}`：查詢自己的工作、進度、費用與結果。
- `meeting_transcribe_cancel {"job_id":"…"}`：取消自己的工作。別人的工作與不存在的工作回覆相同。
- `meeting_upload_link {}`：在原始使用者對話建立一次性上傳連結。沒有可用附件路徑時，`meeting_transcribe` 也會提供連結。

連結必須在 30 分鐘內開始使用，只能建立一次上傳。已開始的連結最長保留 24 小時，但上傳閒置 2 小時即過期，完成後連結失效。它綁定原員工及原對話，只能存取自己的上傳協定。伺服器只保存隨機 token 的雜湊。管理員須設定 HTTPS 的 `[dashboard] public_url`，也支援反向代理子路徑；未設定時請改用控制台頁面。

完成通知包含長度、辨識講者數及檔名。Telegram、Discord、Slack 可附上 Markdown，並沿用原帳號與討論串；LINE 提供附件取得提示。WebChat 通知寫回原對話，重新整理後可見。目前其他通道若沒有可驗證的原傳送目標，會保留通知失敗狀態，不改用其他機器人帳號。通知失敗會退避重試，最多 5 次；逐字稿仍可從控制台取得。

## 設定及服務商

管理員可在「設定 → 語音 → 會議逐字稿」選擇引擎與模型；大小、配額與並行數可設定於 `config.toml`：

```toml
[transcription]
engine = "gemini" # gemini、whisper、google_stt
# model = "gemini/<登錄表中支援音訊與結構化輸出的模型>"
max_bytes = 4294967296 # 1～4294967296，預設 4 GiB
staging_quota_bytes = 10737418240 # 預設 10 GiB，所有工作與上傳共用
media_memory_limit_bytes = 2147483648 # 2 GiB
max_concurrent_jobs = 1 # 只接受 1 或 2
```

Gemini 使用設定的模型，未設定時從 ModelRegistry 選擇支援音訊與結構化輸出的 Gemini 模型；Whisper 使用現有 OpenAI 憑證，不提供講者辨識。Gemini 可能誤聽，且跨段講者標籤可能不一致。員工不能以工具參數更換管理員選定的服務商。

主機需安裝 **ffmpeg 與 ffprobe** 才能抽取影片音軌及自動分段。每段約 30 分鐘，最多 96 段／48 小時；沒有 ffmpeg 時，音訊須符合服務商檔案上限，且 ffprobe 必須確認長度不超過 30 分鐘；長度未知或過長時拒絕轉錄。所有結果經 OpenCC `s2twp` 轉成臺灣用語；這不是外語翻譯。

待處理上傳只預留檔案大小加 64 KiB；每位上傳者、每位員工最多 2 筆，且各自最多占總配額一半。取消或過期立即釋放預留額度。建立工作後另預留來源快照、處理副本、轉檔及輸出空間；完成後保留的逐字稿仍按實際檔案大小計入配額。工作結束後刪除上傳來源與音訊暫存，保留逐字稿；原先透過一般附件提交的原始檔不會刪除。未完成工作在閘道重啟後從頭重跑，輸出使用固定工作編號覆寫，不保證避免重複的服務商費用。已回報費用會累計保留，中斷中的請求可能有無法計量的費用。

工作與音訊只由持有執行鎖的閘道處理。員工暫停會阻止新增及後續段落；權限、外送或遮罩政策變更時，舊工作會停止，須重新提交。控制台／連結上傳遇到需要額外核准的轉錄政策時會拒絕，不會略過核准；這一版尚未把網頁上傳接入互動審批流程。

### Google Cloud Speech-to-Text v2

`google_stt` 使用 `asia-southeast1` 的 `chirp_2`，語言預設為 `cmn-Hant-TW`。[Google 的 Chirp 2 文件](https://docs.cloud.google.com/speech-to-text/docs/models/chirp-2)列出臺灣華語與批次辨識，但不支援講者分離。本引擎目前只接受 `cmn-Hant-TW` 與 `chirp_2`，並拒絕開啟講者分離，不會悄悄忽略。地區限 `asia-southeast1`、`us-central1`、`europe-west4`。每段音訊在本機限制為 25 MiB；ffmpeg 產生的 30 分鐘 AAC 分段可符合此上限。單字時間對應到逐字稿段落，講者欄位留空。管理員設定的語言優先於個別工作的語言提示。

1. 在已啟用帳單的 Google Cloud 專案啟用 Cloud Speech-to-Text 與 Cloud Storage，建立相容地區的私人儲存桶。
2. 建立專用服務帳戶，授予 Speech Client（`roles/speech.client`）及限於該儲存桶的物件建立、讀取與刪除權限。必要時（尤其跨專案）也要授予該專案的 Speech 服務代理讀取儲存桶的權限。不使用 Workspace OAuth 或 Gemini API key。
3. 在「設定 → 語音 → 會議逐字稿」選擇 Google Cloud Speech-to-Text，填入專案 ID、地區、儲存桶名稱（不含 `gs://`）、語言與 `chirp_2`。將服務帳戶 JSON 貼入只供寫入的憑證欄位；閘道沿用機器金鑰加密，留白保留現有憑證。金鑰不會回傳控制台或員工，按「移除已儲存的金鑰」可清除。控制台使用閘道專屬的 `transcription_credentials.set`（`google_stt_key`）與 `.clear` RPC；兩者都在 extension 路由前確認目前仍為管理員。通用 `system.update_config` 拒絕憑證欄位；原始設定編輯器只允許保留遮罩或清除，不接受新增明文或自行提供的密文，並驗證轉錄設定。儲存時只做本機格式檢查，尚未驗證真實 IAM 權限。
4. 先提交短錄音，再驗收長錄音、權限及帳單。本次實作未使用真實 Google 服務驗收。

```toml
[transcription.google_stt]
project_id = "your-project-id"
location = "asia-southeast1"
bucket = "your-private-meeting-bucket"
language = "cmn-Hant-TW"
model = "chirp_2"
diarization = false
```

[BatchRecognize 的音訊輸入必須放在 GCS](https://docs.cloud.google.com/speech-to-text/docs/batch-recognize)，inline 只省去結果儲存桶。每個音訊片段上傳前，閘道會以檔案鎖保護的持久化紀錄，在 home 目錄記下儲存桶、隨機物件名稱、generation（上傳回覆前為未知）與清理狀態，檔案位於 `<home>/google_stt_cleanup/<uuid>.json`，不含音訊或憑證。上傳及辨識期間持續持有 OS 鎖，掃描會跳過；已知 generation 時只刪除該版本。上傳回覆遺失時，即使提早刪除成功或收到 HTTP 404，仍保留紀錄至 900 秒等待期結束後再次確認。暫時性失敗每輪最多嘗試三次，間隔 1、2 秒；待清理紀錄會保留至重啟後，並在啟動及每 60 秒掃描時重試，直到刪除成功或回傳 HTTP 404。清理僅限目前設定的儲存桶；不符的紀錄會原樣保留，警告只記數量。變更儲存桶前請先完成清理，或恢復先前的儲存桶設定後重試；單獨切換引擎不會停止清理。清理完成前仍須保留有效憑證與刪除權限。Unix 清理目錄權限為 0700，紀錄及鎖檔為 0600，拒絕符號連結與非一般檔案。請同時設定儲存桶生命週期到期規則，作為憑證失效或長期清理失敗的備援；取消不會撤回已送出的辨識或費用。輪詢有時間上限，重啟可能重複產生費用。費用以 V2 標準牌價每分鐘 0.016 美元、秒數無條件進位估算，未計折扣、儲存、網路與其他雲端費用；紀錄標為估算，不能當成帳單。參考 [Google 定價](https://cloud.google.com/speech-to-text/pricing)。辨識品質、地區可用性、真實 IAM、清理及費用仍待活體驗證。


## 指令列與隱私

本機操作者仍可同步執行：

```sh
duduclaw meeting transcribe ./meeting.mp4 --out ./transcripts
```

此 CLI 拒絕 AI 員工工作階段；共用 Bash 操作者指令檢查也會攔截員工與未驗證呼叫者。員工請使用會議 MCP 工具。`write_wiki: true` 尚未支援；請檢查結果後再使用既有知識庫工具。

音訊會傳送到選定的服務商。Gemini 免費 API 方案內容可能用於改善產品，請依使用方案與會議保密要求確認。隱私遮罩在逐字稿及來源名稱落盤前執行，失敗時不儲存；它不會移除已傳送音訊中的敏感語音。員工可要求開啟遮罩，不能關閉全域遮罩。

真實服務商品質、真實長片的瀏覽器轉碼、跨平台 ffmpeg、實際通道附檔與大檔網路續傳仍需獨立驗收；mock 測試不代表這些活體流程已通過。

## 權限與資源限制

讀取狀態、清單及下載需要 Viewer；建立、上傳分段、完成及取消需要 Operator。提交時由閘道保存私有快照，核准與外送綁定 SHA-256 及大小，工作只讀取並驗證該快照。AI 狀態工具僅提供狀態、進度、費用、附件路徑及數量，不回傳逐字稿文字；預覽留在控制台，完成通知不含內容摘錄。

每項工作上限為 200,000 段、文字加講者 32 MiB、輸出 256 MiB。上傳與下載分開限流，同一上傳者同時只能處理一個傳輸；30 秒視窗低於 32 KiB/s 會中止。公開連結先通過 IP／全域限流才查詢 token，驗證後再按連結限流。稽核只保存遮罩後識別及固定結果碼，不記錄 token、金鑰或逐字稿。Whisper 一律按本機量測的分段長度計費；Gemini 音訊用量低於每秒 16 tokens 時，改用本機估算並標為估計值。會議工具呼叫稽核只留附件標籤與驗證過的工作編號前綴，不保存上傳連結或結果文字。

Unix 媒體子行程另限制 CPU 時間與 256 個檔案描述元。Linux 限制位址空間，macOS 嘗試 AS／DATA 限制；兩者另每 500 毫秒檢查 RSS，系統不支援 rlimit 時也會執行。超過 `media_memory_limit_bytes`（預設 2 GiB）會終止整個行程群組並讓工作失敗；無法檢查 RSS 時也會停止工作。
