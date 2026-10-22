# Knowledge-base embeddings: zh-TW eval and throughput (2026-10-22)

> L2 internal. Branch `feat/knowledge-embeddings` (PR3 of the knowledge-base
> P0 plan). Numbers from one machine; read the caveats before quoting them.

## Setup

- Corpus: `crates/duduclaw-knowledge/tests/fixtures/eval-zh50/` — five
  fictional zh-TW documents (handbook .md, travel policy .md, security
  policy .txt, product FAQ .txt, 30-row price list .csv), 106 passages after
  chunking; 50 questions (15 factoid, 10 table, 8 paraphrase with no shared
  terms, 7 short terms / codes, 5 multi-concept, 5 unanswerable). Written
  for this test, so not a substitute for a customer's documents.
- Command (debug build):
  `duduclaw knowledge eval --docs …/eval-zh50/docs --questions …/eval-zh50/questions.jsonl --model local:<id> --modes keyword,vector,hybrid --work-dir <scratch>`
- Machine: Apple M4, 10 cores, 16 GB, macOS; ORT 1.24.2 CPU, 5 intra-op
  threads (`threads = 0` ⇒ half the cores). Another work package was
  compiling Rust on the same machine during the runs.
- Models: multilingual-e5-small int8 (`intfloat/multilingual-e5-small` @
  `614241f6`, `onnx/model_qint8_avx512_vnni.onnx`), Qwen3-Embedding-0.6B int8
  (`onnx-community/Qwen3-Embedding-0.6B-ONNX` @ `c25a394d`,
  `onnx/model_int8.onnx`, first 512 of 1024 dims, one text per pass).

## Retrieval (45 answerable questions)

| model | mode | R@1 | R@5 | R@10 | MRR@10 | paraphrase R@5 | table R@5 | short R@5 | query p50 / p95 ms | misses (top 10) |
|---|---|---|---|---|---|---|---|---|---|---|
| — | keyword | 0.67 | 0.78 | 0.80 | 0.72 | 0.38 | 0.90 | 0.71 | 1.2 / 3.3 | q02, q03, q26, q27, q28, q31, q33, q36, q40 |
| e5-small | vector | 0.76 | 0.87 | 0.91 | 0.80 | 0.75 | 0.70 | 1.00 | 4.8 / 6.7 | q09, q18, q29, q30 |
| e5-small | **hybrid** | 0.71 | **0.96** | 0.96 | 0.80 | 0.88 | 0.90 | 1.00 | 5.6 / 7.2 | q18, q33 |
| Qwen3-0.6B | vector | 0.78 | 0.96 | 0.96 | 0.83 | 1.00 | 0.90 | 1.00 | 49.9 / 57.4 | q13, q21 |
| Qwen3-0.6B | **hybrid** | 0.76 | **0.93** | 0.96 | 0.83 | 0.88 | 0.90 | 1.00 | 49.9 / 60.0 | q21, q33 |

Unanswerable questions: every mode returned something for all five (no
threshold yet; the "no answer in the knowledge base" threshold is PR4).

## Throughput (embedding all 106 passages)

| model | run 1 | run 2 | RSS peak (run 2) |
|---|---|---|---|
| e5-small | 1,209 chunks/min | 2,476 chunks/min | 0.89 GB |
| Qwen3-0.6B | 156 chunks/min | 342 chunks/min | 1.73 GB |

Run 1 overlapped a heavy Rust build; run 2 less so. Treat the range as the
uncertainty. Qwen3 is about 7× slower per passage than e5 here.

## Decision rule (owner decision D3)

Switch the default to Qwen3 only if hybrid Recall@5 is ≥ +5 points over e5
**and** a 16 GB CPU mini PC does ≥ 30 chunks/min. Hybrid Recall@5: Qwen3
0.93 vs e5 0.96 (−3 points) ⇒ **e5 stays the default**; Qwen3 is offered
as an option. The mini-PC throughput leg was not measured (no such machine
here); on this M4 both models clear 30 chunks/min by a wide margin.

## Other measurements

- Pooling checks against the real models
  (`crates/duduclaw-knowledge/tests/local_model_live.rs`): Qwen3 reproduces
  the model card's example similarity matrix within 0.07
  ([[0.759, 0.189], [0.198, 0.616]] vs card [[0.7646, 0.1414], [0.1355,
  0.6000]], int8 vs fp32); a text embedded alone vs next to a long one:
  cosine 0.9994 (e5, batched), 1.0000 (Qwen3, one per pass). Batched Qwen3
  gave 0.965, hence batch 1 for it.
- Live gateway (isolated home, port 18897): online install of e5 + ORT (167 MB)
  finished within the first two 3-second status polls and the 26 passages were
  embedded about 3 s later; reindex reused every vector
  (`embedded=0`); offline bundle install via "Check again" worked; removing
  the model returned keyword-only with the reason; a cloud provider against a
  local mock received only de-identified text, never the `restricted`
  collection, and nothing at all before redaction was enabled.

## Not measured

16 GB mini-PC CPU throughput; Linux / Windows model load; customer
documents; real OpenAI / Gemini responses; latency at 20k+ passages.
