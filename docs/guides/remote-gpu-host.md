# Preparing a remote GPU host

Your DuDuClaw machine curates the dataset; a machine with a discrete GPU does the training. This is the ten-minute setup for that second machine, for use with **Manage → Fine-tuning → Training jobs → Your own GPU host** (see [Fine-tuning and post-training](../features/54-finetune.md)).

## What the host needs

| | Requirement |
|---|---|
| OS | Ubuntu 22.04 or 24.04 (any distro with a working CUDA stack is fine; these are the tested ones) |
| GPU | NVIDIA with 16 GB VRAM or more for a 7–8B LoRA; 24 GB is comfortable |
| Driver | NVIDIA driver 550+ with CUDA 12.x |
| Disk | 60 GB free — base model weights plus checkpoints |
| Access | SSH reachable from the DuDuClaw machine, key-based login |

An 8B LoRA on 3 epochs of a few thousand samples typically runs 30–90 minutes on a 24 GB card. Numbers vary with your data; measure rather than plan against these.

## 1. Check the GPU

```bash
nvidia-smi
```

You should see the driver version and free VRAM. If this command is missing, install the driver first — nothing below will work without it.

## 2. Install LLaMA-Factory in a virtualenv

A virtualenv keeps this off the system Python and gives the gateway one stable interpreter path to point at.

```bash
sudo apt update && sudo apt install -y python3-venv git
sudo mkdir -p /opt/llamafactory && sudo chown "$USER" /opt/llamafactory
python3 -m venv /opt/llamafactory/venv
/opt/llamafactory/venv/bin/pip install --upgrade pip
/opt/llamafactory/venv/bin/pip install "llamafactory[torch,metrics]"
```

Verify:

```bash
/opt/llamafactory/venv/bin/llamafactory-cli version
```

The gateway looks for `llamafactory-cli` **next to the Python interpreter you configure**, so keeping both in `/opt/llamafactory/venv/bin/` is what makes the default work.

## 3. Create the working directory

```bash
sudo mkdir -p /srv/duduclaw-train && sudo chown "$USER" /srv/duduclaw-train
```

Each job gets its own subdirectory here: the dataset under `data/`, the generated `train.yaml`, `train.log`, and the adapter under `output/`.

## 4. (Optional) llama.cpp, for GGUF conversion

Skip this and you get a LoRA adapter back. Add it and the finished job also converts and returns a GGUF.

```bash
sudo mkdir -p /opt/llama.cpp && sudo chown "$USER" /opt/llama.cpp
git clone --depth 1 https://github.com/ggml-org/llama.cpp /opt/llama.cpp
/opt/llamafactory/venv/bin/pip install -r /opt/llama.cpp/requirements.txt
ls /opt/llama.cpp/convert_lora_to_gguf.py
```

That last file must exist — it is what the conversion step calls. If it is absent, the job says so and returns the adapter alone rather than pretending a GGUF was produced.

## 5. Authorise the DuDuClaw machine

On the DuDuClaw machine, create a key dedicated to this and copy the public half over:

```bash
ssh-keygen -t ed25519 -f ~/.ssh/duduclaw_train -N ""
ssh-copy-id -i ~/.ssh/duduclaw_train.pub trainer@gpu.example.com
```

Then confirm a password-free login works — the gateway connects with `BatchMode=yes`, so a host that still asks for anything fails immediately with a clear message instead of hanging:

```bash
ssh -i ~/.ssh/duduclaw_train -o BatchMode=yes trainer@gpu.example.com true
```

`rsync` must also be installed on both machines; it is what moves the dataset and brings the artifacts back.

## 6. Fill the form

In **Manage → Fine-tuning → Training jobs**, choose "Your own GPU host" and enter:

| Field | Example |
|---|---|
| Host address | `gpu.example.com` |
| Login user | `trainer` |
| Private key path | `/home/kai/.ssh/duduclaw_train` (on the DuDuClaw machine) |
| Remote working directory | `/srv/duduclaw-train` |
| Remote Python path | `/opt/llamafactory/venv/bin/python` |
| Remote llama.cpp directory | `/opt/llama.cpp` (leave blank if you skipped step 4) |

These values are validated against a strict character allowlist before they are used, because they end up inside a command that runs on your host. A rejected value means a typo, not a limitation — use plain host names and absolute paths.

## 7. Dry run first

Pick **Dry run** as the backend once before spending GPU time. It validates every setting and writes the exact `train.yaml` and command the real run would use, without uploading anything. Read the plan, then switch the backend to your GPU host and submit.

## Choosing a base model and template

The `template` field must match the base model's chat format, or you will train a fluent model that ignores its own turn structure. Common pairs:

| Base model family | `template` |
|---|---|
| Qwen 2.5 / Qwen 3 | `qwen` |
| Llama 3.x | `llama3` |
| Gemma 2 / 3 | `gemma` |
| Mistral / Ministral | `mistral` |

LLaMA-Factory's own documentation lists the full set. When in doubt, match the model card.

## When something goes wrong

| Symptom | Cause |
|---|---|
| "Could not reach the training host" | SSH key not authorised, host down, or firewall. Re-run the step 5 check. |
| `llamafactory-cli not found` | Wrong Python path, or the install in step 2 failed. Run the step 2 verify command. |
| Job ends immediately as failed | Open the training log on the job card — it is the real `train.log` tail. Out-of-memory and a wrong model id both show up there plainly. |
| Adapter returned but no GGUF | `convert_lora_to_gguf.py` is missing or the conversion failed; the job says which. Step 4 fixes it. |

## Related

- [Fine-tuning and post-training](../features/54-finetune.md) — what the three tabs do and where the privacy gate sits
- [Local model marketplace](../features/45-local-model-marketplace.md) — where an imported GGUF ends up
