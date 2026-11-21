# Converting Simplified-Chinese memories to Traditional Chinese (v1.84.0)

Before v1.84.0 the memories an AI employee writes on its own (key facts, conversation memories) followed the language of the model's output, so an installation that works in Traditional Chinese could end up with some memories in Simplified characters. From v1.84.0 the **memory language** setting decides the language of new memories (see [Dashboard settings](dashboard-settings.md#memory-language-and-background-work-v1840)), and Traditional Chinese output is normalised before it is stored. Memories saved before the upgrade stay as they were. `duduclaw memory convert-script` converts those existing memories.

The command is meant for an operator on the machine that runs DuDuClaw. It is not listed in `duduclaw --help` (the `memory` command group is hidden), but it can be run as shown below.

## What it changes and what it leaves alone

- It converts only two kinds of text in `<home>/memory.db`: key facts and memory entries. Wiki and knowledge pages, conversation summaries and pinned instructions are not touched.
- An entry is converted only when it contains Simplified characters. Traditional Chinese as written in Taiwan is left exactly as it is.
- The conversion uses the OpenCC phrase dictionary bundled with DuDuClaw (Simplified to Taiwan Traditional) and reads characters by phrase, so 头发 becomes 頭髮 while 发现 becomes 發現. No model is called and nothing is translated: wording is not replaced (软件 becomes 軟件, not 軟體; 信息 stays 信息), and English or Japanese memories stay in their language.
- The Taiwan forms 台, 帳 and 濕 are used. If the entry itself already contains 臺, 賬 or 溼, that form is kept and also used for the part being converted.
- An entry that contains Japanese kana is skipped entirely and counted as `skipped_japanese`.
- URLs (`scheme://…`, `mailto:`, `www.`), Markdown link targets, inline code, fenced code blocks and explicit file paths (starting with `/`, `./`, `../`, `~/`, `\\` or a drive letter such as `C:\`) are kept exactly as they are.
- Only vectors produced by the built-in n-gram hash embedder are rebuilt. Vectors produced by any other embedding model are cleared; those memories take no part in similarity checks until they are embedded again (the output says so when it happens).

Examples (from the converter's tests):

| Before | After |
|---|---|
| 用户要求：这个项目的预算是三万元，负责人是张经理 | 用戶要求：這個項目的預算是三萬元，負責人是張經理 |
| 账户资料 | 帳戶資料 |
| 头发、发现、软件、信息 | 頭髮、發現、軟件、信息 |
| 用户预算三万元 `回复` /tmp/预算.txt | 用戶預算三萬元 `回复` /tmp/预算.txt |

## Which employees are converted

| Selection | Behaviour |
|---|---|
| `--all` | Every employee whose resolved memory language is exactly `zh-TW`. Others are listed as skipped, with the language that was resolved. Namespaces without an employee directory are skipped too (use `--agent` for those). |
| `--agent <id>` | That employee's memories. Refused when its resolved memory language is `zh-CN` or Japanese, unless you add `--force-language-mismatch` (it works only together with `--agent`). Employees set to `en`, or with no language resolved, are converted without the extra flag. |

The resolved language is worked out the same way as for new memories: the employee's memory language, then the global memory language, then the employee's reply language (or the global default language).

## Steps

1. Preview. Without `--yes` the command only counts and shows up to ten before/after samples:

   ```bash
   duduclaw memory convert-script --all
   ```

   Each employee gets one line such as `alice: preview key_facts=12, memories=40, changed=52, skipped_japanese=1, embeddings_cleared=0`.

2. Apply:

   ```bash
   duduclaw memory convert-script --all --yes
   ```

   Before the first change the command writes a restore file and prints its path (`Restore file: …`). The file is `<home>/backups/memory-convert-script-<UTC time>-<employee>.jsonl`, readable only by the owner. Each employee is converted in its own transaction; if one fails, the others continue and the command ends with an error.

3. If you need to undo it:

   ```bash
   duduclaw memory convert-script --restore <home>/backups/memory-convert-script-…jsonl
   ```

   A restore puts back the whole file or nothing. It is refused when any of the converted entries was edited after the conversion.

## Flags

| Flag | Meaning |
|---|---|
| `--agent <ID>` | Only this employee's entries. |
| `--all` | Every employee whose memory language resolves to Traditional Chinese. |
| `--restore <FILE>` | Undo an earlier conversion. Cannot be combined with `--agent`, `--all`, `--dry-run`, `--yes` or `--force-language-mismatch`. |
| `--force-language-mismatch` | With `--agent` only (refused together with `--all` or `--restore`): convert even though the employee is set to Simplified Chinese or Japanese. With `--all`, employees whose memory language is not Traditional Chinese are always skipped. |
| `--dry-run` | Preview only. Wins over `--yes` if both are given. |
| `--yes` | Apply the conversion. |

Exactly one of `--agent`, `--all` and `--restore` is required.

## Limits

- The command refuses to run inside an AI employee session and records the refusal in the security audit log (`memory_script_conversion_refused`); employees also cannot start it through their shell tool. Applied conversions and restores are recorded as `memory_script_converted` and `memory_script_restored`.
- Wording is never changed, so mainland vocabulary (软件 → 軟件, 信息, 项目 → 項目) stays mainland vocabulary in Traditional characters.
- In an entry that mixes both scripts, characters shared by both scripts are read by phrase context and may change form, for example 了解 becomes 瞭解. A wrong phrase guess is possible; check the preview samples before applying.
- If the bundled dictionary cannot be loaded, the command falls back to the older character table and leaves characters whose form depends on context unchanged.
- Pages and summaries are not converted. New ones follow the memory language setting from v1.84.0 on.
