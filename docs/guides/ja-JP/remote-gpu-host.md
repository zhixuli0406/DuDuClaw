# 遠隔 GPU 機の準備

DuDuClaw の機械はデータセットを整え、独立グラフィックスを持つ機械が学習を行います。本稿はその二台目の、十分ほどで終わる準備手順です。**管理 → ファインチューニング → 学習ジョブ → 自前の GPU 機**と合わせて使います（[ファインチューニングと事後学習](../../features/ja-JP/54-finetune.md)を参照）。

## 必要なもの

| | 条件 |
|---|---|
| OS | Ubuntu 22.04 または 24.04（CUDA が動く配布版なら何でも構いませんが、検証済みはこの二つ） |
| GPU | NVIDIA。7–8B の LoRA なら VRAM 16 GB 以上、24 GB あれば余裕 |
| ドライバ | NVIDIA driver 550 以上、CUDA 12.x |
| ディスク | 空き 60 GB——ベースモデルの重みとチェックポイント |
| 接続 | DuDuClaw 機から SSH で到達でき、鍵でログインできること |

8B の LoRA を数千件のサンプルで 3 エポック回すと、24 GB のカードで概ね 30–90 分です。データ次第で変わるので、この数字を前提に計画せず実測してください。

## 1. GPU を確認する

```bash
nvidia-smi
```

ドライバのバージョンと空き VRAM が見えるはずです。このコマンドが無ければ先にドライバを入れてください。以降はすべてその上に乗っています。

## 2. 仮想環境に LLaMA-Factory を入れる

仮想環境にするのは、システムの Python を汚さないためと、ゲートウェイに指し示す安定したインタプリタのパスを一つ用意するためです。

```bash
sudo apt update && sudo apt install -y python3-venv git
sudo mkdir -p /opt/llamafactory && sudo chown "$USER" /opt/llamafactory
python3 -m venv /opt/llamafactory/venv
/opt/llamafactory/venv/bin/pip install --upgrade pip
/opt/llamafactory/venv/bin/pip install "llamafactory[torch,metrics]"
```

確認：

```bash
/opt/llamafactory/venv/bin/llamafactory-cli version
```

ゲートウェイは**設定した Python インタプリタの隣**で `llamafactory-cli` を探します。両方を `/opt/llamafactory/venv/bin/` に置いておくことで、既定値がそのまま通ります。

## 3. 作業ディレクトリを作る

```bash
sudo mkdir -p /srv/duduclaw-train && sudo chown "$USER" /srv/duduclaw-train
```

ジョブごとにこの下へ専用のサブディレクトリができます。データセットは `data/`、生成された `train.yaml` と `train.log`、adapter は `output/` です。

## 4.（任意）GGUF 変換用の llama.cpp

省略すると LoRA adapter だけが返ります。入れておくと、完了時に GGUF も変換して返します。

```bash
sudo mkdir -p /opt/llama.cpp && sudo chown "$USER" /opt/llama.cpp
git clone --depth 1 https://github.com/ggml-org/llama.cpp /opt/llama.cpp
/opt/llamafactory/venv/bin/pip install -r /opt/llama.cpp/requirements.txt
ls /opt/llama.cpp/convert_lora_to_gguf.py
```

最後のファイルは必ず存在する必要があります。変換処理が呼ぶのはこれです。無い場合、ジョブはそう明言し、GGUF を作ったふりをせずに adapter だけを返します。

## 5. DuDuClaw 機を許可する

DuDuClaw 機側で専用の鍵を作り、公開鍵を送ります。

```bash
ssh-keygen -t ed25519 -f ~/.ssh/duduclaw_train -N ""
ssh-copy-id -i ~/.ssh/duduclaw_train.pub trainer@gpu.example.com
```

パスワード無しでログインできることを確認します。ゲートウェイは `BatchMode=yes` で接続するため、何かを尋ねてくる相手には即座に明確なメッセージで失敗し、画面が固まることはありません。

```bash
ssh -i ~/.ssh/duduclaw_train -o BatchMode=yes trainer@gpu.example.com true
```

`rsync` は両方の機械に必要です。データセットの送信と成果物の回収に使います。

## 6. フォームを埋める

**管理 → ファインチューニング → 学習ジョブ**で「自前の GPU 機」を選び、次を入力します。

| 欄 | 例 |
|---|---|
| ホスト | `gpu.example.com` |
| ログインユーザー | `trainer` |
| 秘密鍵のパス | `/home/kai/.ssh/duduclaw_train`（DuDuClaw 機上） |
| 遠隔の作業ディレクトリ | `/srv/duduclaw-train` |
| 遠隔の Python パス | `/opt/llamafactory/venv/bin/python` |
| 遠隔の llama.cpp ディレクトリ | `/opt/llama.cpp`（手順 4 を飛ばしたなら空欄） |

これらの値は、あなたのホスト上で実行されるコマンドの中に入るため、使用前に厳格な文字の許可リストで検証されます。弾かれたら制限ではなく打ち間違いです。素直なホスト名と絶対パスを使ってください。

## 7. まずドライラン

GPU の費用を使う前に、一度バックエンドを**ドライラン**にして走らせてください。設定をすべて検証し、本番と同じ `train.yaml` とコマンドを書き出しますが、何も送信しません。計画を読んで問題なければ、バックエンドを GPU 機に切り替えて送信します。

## ベースモデルとテンプレートの選び方

`template` はベースモデルの対話形式と一致させる必要があります。ずれると、流暢なのに自分のターン構造を無視するモデルが出来上がります。よくある対応：

| ベースモデル系統 | `template` |
|---|---|
| Qwen 2.5 / Qwen 3 | `qwen` |
| Llama 3.x | `llama3` |
| Gemma 2 / 3 | `gemma` |
| Mistral / Ministral | `mistral` |

全一覧は LLaMA-Factory 自身のドキュメントにあります。迷ったらモデルカードの記述に合わせてください。

## うまくいかないとき

| 症状 | 原因 |
|---|---|
| 「学習ホストに一時的につながりません」 | SSH 鍵が未登録、ホストが停止、あるいはファイアウォール。手順 5 の確認をやり直してください。 |
| `llamafactory-cli not found` | Python パスの誤り、または手順 2 のインストール失敗。手順 2 の確認コマンドを実行してください。 |
| 送信直後に失敗する | ジョブカードの学習ログを開いてください。本物の `train.log` の末尾です。メモリ不足もモデル ID の誤りもそこに素直に出ます。 |
| adapter は返るが GGUF が無い | `convert_lora_to_gguf.py` が無いか変換に失敗しています。どちらかはジョブが明示します。手順 4 で解決します。 |

## 関連

- [ファインチューニングと事後学習](../../features/ja-JP/54-finetune.md)——三つのタブの役割と、データが機械を出るときの関門
- [ローカルモデルマーケット](../../features/ja-JP/45-local-model-marketplace.md)——取り込んだ GGUF が現れる場所
