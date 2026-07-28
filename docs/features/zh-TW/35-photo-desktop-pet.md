# 照片 → 互動桌面寵物

> 丟進一張照片,得到一隻在螢幕上遊蕩的像素風桌面夥伴——完全在你的機器上生成,不用雲端影像模型。

---

## 這是什麼

桌面 app(v1.46)把一張照片——你的貓、小孩的塗鴉、一隻絨毛玩偶——變成會動、永遠置頂的桌面寵物。整條管線都在本地:去背、像素風轉換、spritesheet 烘焙全部跑在 `duduclaw-pets` crate 裡,零外部 API 呼叫。之後寵物住進一個小小的透明視窗,播放 idle 動畫、自己在桌面上遊蕩,並對 agent 活動做出反應。

## 生成管線

```text
photo → EXIF orientation fix → background removal → RGBA cutout
      → pixel quantization → 8×9 spritesheet bake → pet pack on disk
```

1. **去背**(`segmentation.rs`):本地 ONNX 推論,主用 BiRefNet-general-lite,低資源 fallback 用 silueta,藏在 `onnx` feature 後面。`PassthroughRemover` 永遠可用——它支撐「我已經自己去好背了」的流程,並保證沒裝任何模型也能生成。模型放在 `~/.duduclaw/models/`(與 GGUF 倉庫共用),按需下載,絕不在建置時下載。手機照片會先套用 EXIF `Orientation` 標籤,直式照片不會橫著進來。
2. **像素量化**(`pixelate.rs`):nearest-neighbour 縮小到 64 像素寬的標準網格,硬 alpha 門檻(像素風就是硬邊),再以 median-cut 色彩量化壓到 16 色調色盤——結果讀起來是一張限色 sprite,不是縮小的照片。
3. **Spritesheet 烘焙**(`sprite_bake.rs`):單張 sprite 只用確定性的幾何變換(scale / translate / flip / shear)展開成 8 欄 × 9 列的動畫幀網格——不用繪圖模型。列的排列遵循 openpets / Codex Pets 佈局(idle、running-right、running-left、waving、jumping、failed、waiting、running、review),幀格 192×208,烘出來的 sheet 可直接投入該生態系使用。

包落在 `~/.duduclaw/pets/<slug>/`,帶一份 `pet.json` manifest(Codex Pets 的超集)。原始照片原樣保留(`source.png`),之後重新生成寵物不必重新上傳。

## 兩種寵物模式

| 模式 | 來源 | 動畫 |
|---|---|---|
| `procedural` | 一張去背後的 cutout | WAAPI keyframes + 手刻 spring,作用在單張圖上 |
| `sprite` | 烘好的 8×9 spritesheet | 在 `<canvas>` 上按狀態逐幀播放,nearest-neighbour 縮放 |

procedural 路徑是 P0 流程(快,任何照片都行);sprite 路徑是像素風流程。兩者由同一套 runtime 與同一個互動狀態機驅動。

## 視窗

`mascot_window.rs` 建出第二個 Tauri 視窗:透明、無邊框、永遠置頂、跳過工作列,基準尺寸 180 邏輯 px,tray 切換前保持隱藏。macOS 上停用了視窗自身的陰影——否則透明無裝飾視窗仍會畫出一圈勾勒邊界的矩形陰影,也就是使用者回報的「寵物外面有個框」。只剩寵物自己的像素可見。

## 遊蕩引擎

閒置時,`PetRuntime.tsx` 週期性挑一個加權隨機行為:

- **往左 / 往右走** — 透過 `petMoveBy` 移動*真正的*視窗橫越桌面,碰到螢幕邊緣就反向折返。
- **休息**(坐下)、**揮手**、**跳躍** — 定時的原地動畫。

任何互動或 agent 訊號都會立即打斷遊蕩:每個行為在行動前都重查即時狀態,按壓或拖曳能在走到一半時接管。所有自主移動都受 `prefers-reduced-motion` 管制,且在挑選行為的當下檢查,設定即改即生效。在 Tauri 之外(純瀏覽器),走路降級為原地動畫,不移動視窗。

## 互動

- **拖曳** — 一個手勢(mousedown + 移動超過 4 px 才啟動原生視窗拖曳),刻意不用 `data-tauri-drag-region`,所以單純點一下仍算點擊(Tauri #9751/#9901)。放開時觸發墜落動畫。
- **點擊** — 一段反應動畫;60 秒沒有互動,寵物就打起瞌睡。
- **右鍵** — 原生 context menu,含「開啟 studio」(把主視窗導到 pet studio)與尺寸選擇。
- **縮放** — 小 / 標準 / 大(180 px 基準的 50% / 100% / 150%)。宿主調整的是*視窗*大小;寵物跟隨 viewport,縮放永遠不會長出看不見的外框。

## Agent 訊號

寵物兼作狀態面。agent 訊號(`working` / `notify` / `idle`)切換牠的狀態:`working` 播忙碌動畫,`notify` 舉起一塊使用者可關閉的告示牌(新項目到達時重新舉起)。待決審批是第一個接上的即時來源——未決的審批請求會舉起 notify 告示牌;`working` 的即時 agent 狀態流是規劃中的下一個掛點。

## Pet Studio

儀表板的 pet studio 列出所有生成過的包(名稱、模式、啟用旗標),可用新照片生成、挑選啟用中的寵物、刪除包。Studio 與 overlay 講同一層薄薄的 Tauri command(`pet_gen.rs`);啟用中的寵物切換時,`pet://changed` 事件廣播到所有視窗,overlay 不必重啟就重新抓取。

## 限制

| 項目 | 值 |
|---|---|
| 像素網格寬度 | 64 px(標準) |
| 調色盤 | 16 色(median-cut) |
| Spritesheet | 8 欄 × 9 列,192×208 px 幀格 |
| 生成期間的雲端呼叫 | 無 |
| 去背模型 | BiRefNet-lite / silueta(可選),passthrough 永遠可用 |
