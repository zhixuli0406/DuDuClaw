# 準備一台遠端 GPU 主機

DuDuClaw 這台機器負責整理資料集，有獨立顯示卡的機器負責訓練。本文是那台機器的十分鐘準備手續，搭配**管理 → 微調與後訓練 → 訓練工作 → 自有 GPU 主機**使用（見[微調與後訓練](../../features/zh-TW/54-finetune.md)）。

## 主機需要什麼

| | 條件 |
|---|---|
| 作業系統 | Ubuntu 22.04 或 24.04（任何 CUDA 能跑的發行版都行，這兩個是實測過的） |
| GPU | NVIDIA，訓練 7–8B LoRA 建議 16 GB 以上 VRAM，24 GB 較寬裕 |
| 驅動 | NVIDIA driver 550 以上，CUDA 12.x |
| 磁碟 | 60 GB 可用空間（基礎模型權重加上 checkpoint） |
| 連線 | DuDuClaw 這台機器能 SSH 連到，且以金鑰登入 |

8B 的 LoRA、幾千筆樣本跑 3 輪，在 24 GB 卡上大約 30–90 分鐘。實際時間視資料而定，請以實測為準，不要拿這裡的數字當計畫。

## 1. 確認 GPU

```bash
nvidia-smi
```

應該看得到驅動版本與剩餘 VRAM。這個指令不存在就先裝驅動。沒有它，底下每一步都不會成立。

## 2. 在虛擬環境裡安裝 LLaMA-Factory

用虛擬環境是為了不污染系統 Python，同時給 gateway 一個穩定的直譯器路徑可以指。

```bash
sudo apt update && sudo apt install -y python3-venv git
sudo mkdir -p /opt/llamafactory && sudo chown "$USER" /opt/llamafactory
python3 -m venv /opt/llamafactory/venv
/opt/llamafactory/venv/bin/pip install --upgrade pip
/opt/llamafactory/venv/bin/pip install "llamafactory[torch,metrics]"
```

驗證：

```bash
/opt/llamafactory/venv/bin/llamafactory-cli version
```

gateway 會到**你填的那個 Python 直譯器旁邊**找 `llamafactory-cli`，所以兩者都留在 `/opt/llamafactory/venv/bin/` 底下，預設值才會直接生效。

## 3. 建立工作目錄

```bash
sudo mkdir -p /srv/duduclaw-train && sudo chown "$USER" /srv/duduclaw-train
```

每個訓練工作在這底下有自己的子目錄：資料集放 `data/`，還有產生的 `train.yaml`、`train.log`，以及放在 `output/` 的 adapter。

## 4.（選用）llama.cpp，用來轉 GGUF

跳過這步的話，你拿回來的是 LoRA adapter。裝了的話，訓練完成後會順便轉出 GGUF 一起帶回。

```bash
sudo mkdir -p /opt/llama.cpp && sudo chown "$USER" /opt/llama.cpp
git clone --depth 1 https://github.com/ggml-org/llama.cpp /opt/llama.cpp
/opt/llamafactory/venv/bin/pip install -r /opt/llama.cpp/requirements.txt
ls /opt/llama.cpp/convert_lora_to_gguf.py
```

最後那個檔案必須存在，轉檔步驟呼叫的就是它。不存在的話，訓練工作會直接講明，只帶回 adapter，不會假裝有 GGUF。

## 5. 授權 DuDuClaw 這台機器

在 DuDuClaw 機器上產生一把專用金鑰，把公鑰複製過去：

```bash
ssh-keygen -t ed25519 -f ~/.ssh/duduclaw_train -N ""
ssh-copy-id -i ~/.ssh/duduclaw_train.pub trainer@gpu.example.com
```

接著確認免密碼登入真的可以。gateway 是以 `BatchMode=yes` 連線的，所以任何還會問東西的主機會立刻以明確訊息失敗，而不是把畫面卡住：

```bash
ssh -i ~/.ssh/duduclaw_train -o BatchMode=yes trainer@gpu.example.com true
```

兩台機器都要裝 `rsync`，資料集送過去與產物拿回來都靠它。

## 6. 填表

在**管理 → 微調與後訓練 → 訓練工作**選「自有 GPU 主機」，填入：

| 欄位 | 範例 |
|---|---|
| 主機位址 | `gpu.example.com` |
| 登入帳號 | `trainer` |
| 私鑰檔案路徑 | `/home/kai/.ssh/duduclaw_train`（在 DuDuClaw 這台機器上） |
| 遠端工作目錄 | `/srv/duduclaw-train` |
| 遠端 Python 路徑 | `/opt/llamafactory/venv/bin/python` |
| 遠端 llama.cpp 目錄 | `/opt/llama.cpp`（跳過第 4 步就留空） |

這些值在使用前會先過一層嚴格的字元白名單，因為它們最後會出現在一段要在你主機上執行的指令裡。被擋下代表打錯字，不是功能限制。用單純的主機名與絕對路徑就好。

## 7. 先跑一次乾跑

花 GPU 錢之前，先把後端選成**乾跑**跑一次。它會驗證每一項設定，並寫出真正會用的那份 `train.yaml` 與指令，全程不上傳任何東西。看過計畫沒問題，再把後端切成你的 GPU 主機送出。

## 基礎模型與模板怎麼選

`template` 必須對上基礎模型的對話格式，否則你會訓練出一個很流暢、但無視自己回合結構的模型。常見搭配：

| 基礎模型家族 | `template` |
|---|---|
| Qwen 2.5 / Qwen 3 | `qwen` |
| Llama 3.x | `llama3` |
| Gemma 2 / 3 | `gemma` |
| Mistral / Ministral | `mistral` |

完整清單見 LLaMA-Factory 自己的文件。不確定就照模型卡上寫的來。

## 出狀況的時候

| 症狀 | 原因 |
|---|---|
| 「暫時連不上訓練主機」 | SSH 金鑰沒授權、主機沒開，或防火牆擋住。重跑第 5 步的檢查。 |
| `llamafactory-cli not found` | Python 路徑填錯，或第 2 步的安裝失敗。跑一次第 2 步的驗證指令。 |
| 工作一送出就失敗 | 打開工作卡片上的訓練日誌，那是真的 `train.log` 結尾。顯存不足與模型代號寫錯都會直接寫在裡面。 |
| 拿回 adapter 但沒有 GGUF | `convert_lora_to_gguf.py` 不存在或轉檔失敗，工作會說明是哪一種。補做第 4 步即可。 |

## 延伸閱讀

- [微調與後訓練](../../features/zh-TW/54-finetune.md)：三個分頁各做什麼，以及資料離機閘門在哪裡
- [本地模型市集](../../features/zh-TW/45-local-model-marketplace.md)：匯入的 GGUF 最後會出現在哪
