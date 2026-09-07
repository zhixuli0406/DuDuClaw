# Fine-tuning and post-training

> Curate the data here, train it on a GPU elsewhere, bring the model back here. This machine never trains.

---

## The claim we refuse to make

A DuDuClaw appliance is a mini-PC with integrated graphics — an Intel N305 or an AMD 8845HS. It cannot train a language model, and no amount of patience changes that: LLaMA-Factory, Unsloth and Axolotl all assume CUDA or ROCm, and llama.cpp's own fine-tune path has been unmaintained for years. Any product that says "fine-tune on your appliance" is either shipping something else or lying.

So this feature is shaped around what the box is genuinely good at. It has months of your conversations, your task results and your approval decisions sitting in local storage, and nothing else in the world has that. Turning it into a training corpus is the valuable half. The GPU is rented, borrowed, or already under your desk.

Three steps, three tabs:

```
   THIS MACHINE              A GPU YOU SUPPLY              THIS MACHINE
+------------------+      +----------------------+      +------------------+
|  1. Curate       |      |  2. Train            |      |  3. Deploy       |
|                  | ---> |                      | ---> |                  |
| conversations    |      | your own GPU host    |      | GGUF / LoRA into |
| task results     |      |   (SSH, LLaMA-       |      | the local models |
| approvals        |      |    Factory)          |      | directory        |
|      |           |      | or a cloud service   |      |                  |
|      v           |      |                      |      |                  |
| SFT + DPO JSONL  |      | or a dry run: no     |      |                  |
+------------------+      | training at all      |      +------------------+
                          +----------------------+
```

---

## Step 1: curate

Open **Manage → Fine-tuning → Datasets**. Name a dataset, choose a format, and press build. Four sources feed it, all of them stores the gateway already keeps:

| Source | Becomes | Why it is a good training row |
|---|---|---|
| Agent conversations | multi-turn SFT rows | how your staff actually answer, in your vocabulary |
| Completed task results | single-turn SFT rows | the brief and the finished work, paired |
| Review verdicts | DPO preference pairs | the *same* task: a round that was sent back, and the round that was accepted |
| Approval decisions | DPO preference pairs | actions of one kind you approved, against ones you denied |

Two output formats, both written the way LLaMA-Factory reads them, plus a `dataset_info.json` that registers the files — so the remote trainer needs no conversion step:

- **ShareGPT** for multi-turn conversation training.
- **Alpaca** for single-turn instruction training.

Rows that would be junk are dropped rather than padded. A conversation with no answer, a task with no result, an approval still pending — none of them produce a row, and the counts you see are what actually landed in the file. A brand-new machine honestly builds a dataset of zero rows.

You can scope a build to particular agents and to a start date, preview the first rows as JSON before committing to anything, and export the file paths.

---

## Step 2: train, somewhere else

**Manage → Fine-tuning → Training jobs.** Pick the dataset, pick where the GPU is.

### Dry run

Validates every setting, writes the exact `train.yaml` a real run would use plus a `plan.json` naming the exact command, and stops. Nothing is uploaded, nothing is trained, and the job state stays `Planned (not trained)` forever — it never drifts to "done". This is the honest way to check your parameters before spending GPU time.

### Your own GPU host

Connects over SSH to a machine you own, copies the dataset across with rsync, and runs `llamafactory-cli train train.yaml` there. LoRA, SFT or DPO, with rank / epochs / learning rate under your control. When training finishes, the adapter is fetched back; if that host also has llama.cpp's `convert_lora_to_gguf.py`, a GGUF is converted and fetched too.

Setting the host up takes about ten minutes — see [Preparing a remote GPU host](../guides/remote-gpu-host.md).

### Together cloud

Uploads the dataset to [Together AI](https://docs.together.ai)'s fine-tuning service and tracks the job by its id. The dataset is converted to Together's own JSONL shapes first (their SFT and DPO formats differ from LLaMA-Factory's), and the converted file is kept alongside the job so "what did this actually train on" stays answerable. Requires a `TOGETHER_API_KEY` in the gateway's environment.

### No invented progress

There is no progress bar on this page, and that is deliberate. A job shows a state and the verbatim tail of the real training log:

- On your own GPU host, the state comes from whether the remote process is alive and whether an adapter file exists. If the host is briefly unreachable, the job says so and keeps its state — a dropped connection is not a failed training run.
- On Together, the state is their own status field, mapped one to one. Their estimate of seconds remaining is passed through untouched when they offer one.
- A dry run stays planned.

---

## Step 3: deploy

**Manage → Fine-tuning → Import.** Point it at a `.gguf`, `.safetensors` or `.bin` — a local path (the finished-job card offers its artifacts with one click) or an `https://` URL. The file lands in the same models directory the [local model marketplace](45-local-model-marketplace.md) scans, so your fine-tune appears in the local models list next to everything else. There is no second registry: the file is the registry.

---

## The privacy gate

Everything in step 1 happens on this machine. Steps 2 and 3 are where that stops being true, so both of the operations that move curated data outward — exporting a dataset, and creating a job on any backend except `dry_run` — refuse until you tick **"I understand this data will leave this machine"**.

The gate fails closed. An absent acknowledgement is a refusal, not a default-yes, and an unrecognised backend counts as remote. The refusal is a structured response the page turns into a consent step rather than an error, so the checkbox is a decision you make once per action with the warning in front of you.

---

## Boundaries

- **No local training, at all.** Not slow local training, not degraded local training. The appliance curates and stores.
- **Judgement about data still belongs to you.** The dataset builder does not redact. If a conversation held a customer's details, that detail is in the JSONL, which is exactly why the export gate exists.
- **Preference pairs from approvals are a weaker signal** than review verdicts — they are two actions of the same kind judged differently, not two answers to one question. Each row records which source it came from so you can filter.
- **The chat template matters.** A wrong `template` value trains fluent nonsense, so the field is explicit rather than guessed.

---

## The takeaway

The scarce ingredient in a private fine-tune is not GPU time; it is a corpus that reflects how *your* business answers. That corpus is already on this machine. This feature turns it into training data, sends it to hardware that can actually train, and brings the result back — and says plainly, at every step, where the computation is happening and where your data went.
