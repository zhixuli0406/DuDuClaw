# DuDuClaw OS 硬體需求與消費級選機指南

DuDuClaw OS 是一套 **x86-64 的 Agent-Native 作業系統**，跑在一般消費級 PC 或迷你主機上——不需要特殊硬體、不需要獨立顯卡，2015 年後的主流 x86 電腦幾乎都能跑。

本文件給要選機、採購或評估相容性的人：先看「硬性條件」（不符就開不了機），再依用途查配置表挑機型。

## 硬性條件（三項，不符就跑不起來）

1. **x86-64 CPU 且支援 AVX2**（＝ x86-64-v3 baseline）
   - Intel：Haswell（2013，第 4 代 Core）以上
   - AMD：Zen（2017）以上（實務上 2015 後的消費級 CPU 幾乎都有 AVX2）
   - ❌ **不支援 arm64**：包括 Apple Silicon Mac、樹莓派、ARM 迷你主機。OS image 是 x86-64 編譯，ARM 機器（或在 ARM Mac 上用 UTM/QEMU 模擬）開不了機或極慢，不是支援路徑。
2. **UEFI 開機**（2012 後的主機板皆有）
   - A/B 原子更新 + systemd-boot 依賴 UEFI；傳統 BIOS-only 機器不支援。
   - Secure Boot 目前可關（自簽鏈尚未強制）。
3. **SSD**（A/B 更新與 gpui 桌面對 IO 敏感；HDD 會很卡）

## 配置表

| 項目 | 最低 | 建議 | 舒適（打遊戲 / 本地 LLM） |
|---|---|---|---|
| **CPU** | x86-64-v3（須 AVX2）：Intel Haswell↑ / AMD Zen↑ | Intel N100 / N305、AMD Ryzen 5 | AMD Ryzen 7 8845HS |
| **記憶體** | 4 GB | 8 GB | 16 GB+ |
| **儲存** | 64 GB SSD | 128 GB SSD | 256 GB+ SSD |
| **顯示 / GPU** | 無需獨顯（CPU 內顯即可） | 內顯 | 內顯 / 獨顯 |
| **韌體** | UEFI | UEFI | UEFI |
| **網路** | 有線 or WiFi（iwd） | — | — |

> **為什麼不需要獨立顯卡？** DuDuClaw OS 目前走 **llvmpipe 純軟體渲染**（CPU 就能畫桌面），這是它對消費級硬體特別友善的關鍵——買最便宜的無獨顯迷你主機也能跑完整圖形桌面。有內顯/獨顯只是未來硬體加速的空間，不是必要。

## 推薦消費級機型（可直接買）

| 檔位 | 機型舉例 | 說明 |
|---|---|---|
| 💰 便宜 | Intel N100 迷你主機（Beelink / GMKtec，約 NT$4,000–6,000） | 被動散熱、省電，跑桌面＋文書＋上網足夠 |
| ⚖️ 均衡 | Beelink SER 系列（Ryzen）、Intel NUC | 效能與價格平衡 |
| 🎯 目標機型 | Intel N305 / AMD Ryzen 8845HS 迷你主機 | 專案鎖定的目標硬體，跑遊戲 / 本地 LLM 較從容 |

## 已知驅動缺口（選機時避開）

以下硬體目前**驅動支援不足**，選機時避開，或另備一張相容的網卡：

- **MT7927 WiFi 晶片**
- **RTL8125 2.5G 有線網卡**

選機前建議確認主機板/迷你主機用的網路晶片型號，避開上述兩顆。

## 為什麼不能用 Mac / ARM 模擬驗證？

DuDuClaw OS 是 x86-64。在 Apple Silicon Mac 上用 UTM/QEMU 模擬 x86-64 是**軟體模擬（TCG）**，會很慢、顯示與輸入未必順，只適合「快速看一眼開機畫面」，**不適合實際使用**。真正的體驗要燒到 USB 插 x86 UEFI 真機，才是原生速度。

## 燒錄開機媒體的正確做法

把出貨的 `.wic`（或 hybrid `.iso`）用 **balenaEtcher 選檔燒到 USB**，或指令：

```bash
sudo dd if=duduclaw-os-*.wic of=/dev/rdiskN bs=4m status=progress
# rdiskN 換成你 USB 的實際編號，先用 diskutil list（macOS）或 lsblk（Linux）確認、注意別選錯碟
```

⚠️ **務必燒成 USB（當硬碟開機），不要燒成光碟 / 用「Boot from ISO」/ QEMU cdrom**——DuDuClaw 的開機鏈在光碟（`/dev/sr0`）路徑上因 kernel `sr` 驅動限制（`GENHD_FL_NO_PART`，光碟不建 GPT 分割節點）無法開機。USB（block device，等同硬碟）才是唯一且足夠的開機媒體，現代 UEFI 機從 USB 開機也是主流。

## 相關文件

- [appliance-build.md](appliance-build.md) — OS image 建置
- [deployment-guide.md](deployment-guide.md) — 部署（服務端）
