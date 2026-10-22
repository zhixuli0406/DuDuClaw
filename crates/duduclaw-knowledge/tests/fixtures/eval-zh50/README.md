# eval-zh50

A fully synthetic, fictional zh-TW (Taiwan usage) corpus for a made-up company, "青嶼科技股份有限公司". It contains no real people, companies, addresses, phone numbers or emails (only example.com).

Purpose: Recall@k regression for keyword, vector and hybrid retrieval over a small document knowledge base. Five documents in `docs/` (handbook .md, travel policy .md, security policy .txt, product FAQ .txt, price list .csv) and 50 questions in `questions.jsonl`.

Question mix: 15 factoid, 10 table (CSV rows), 8 paraphrase (low lexical overlap), 7 short (terms or codes), 5 multi, 5 unanswerable.

Run from the repo root:

    duduclaw knowledge eval --docs crates/duduclaw-knowledge/tests/fixtures/eval-zh50/docs --questions crates/duduclaw-knowledge/tests/fixtures/eval-zh50/questions.jsonl --modes keyword,vector,hybrid --model local:multilingual-e5-small
