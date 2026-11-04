# Meeting transcripts

[繁體中文](zh-TW/73-meeting-transcripts.md) · [日本語](ja-JP/73-meeting-transcripts.md)

Convert downloaded Meet, Zoom or Teams recordings into Taiwan Traditional Chinese Markdown and JSON, with timestamps and optional speaker labels. This processes completed files; it does not join live meetings or download Drive recordings. No OAuth scopes are added.

## Upload and follow progress

Open **Meeting transcripts**, choose an employee you may use, and select audio or video. Non-admin accounts are restricted to directly bound, selectable employees with Operator/Owner access; main/default and administrator-only machine employees remain unavailable. Files use 8 MiB chunks with a server-verified SHA-256 per chunk. Select the same file after an interruption to resume. Incomplete uploads expire after two hours without a new chunk.

When WebCodecs supports the recording, the browser attempts to extract MP4/MOV AAC-LC audio and encode mono AAC at about 48 kbps. WebM, unsupported codecs or any conversion error fall back to the original file. The page reports the selected path. The demuxer is an exact-pinned mp4box.js release under BSD-3-Clause. Browser conversion is best effort.

The job list shows queued, preparing, transcribing with chunk progress, done, failed or cancelled. Download Markdown and JSON after completion. Cancellation prevents subsequent chunks; an already submitted provider request finishes and records its available usage. Cancellation cannot undo provider charges.

## Chat tools

```json
{"source_path":"meeting.mp4","language_hint":"zh-TW","write_wiki":false}
```

`meeting_transcribe` immediately returns a `job_id` and state. The source must be in the verified employee's attachments. Use `meeting_transcribe_status {"job_id":"…"}` or `meeting_transcribe_cancel {"job_id":"…"}` for that employee's own jobs. Another employee's ID and a nonexistent ID have the same not-found response. Tools are available only in gateway-authenticated employee processes.

`meeting_upload_link {}` creates a single-upload link for the live requesting conversation; a missing or unusable `source_path` also returns a link. Redeem it within 30 minutes. Once admitted, the link lasts at most 24 hours, subject to the upload’s two-hour idle expiry; completion invalidates the link. Its random token is stored hashed and grants only that upload protocol. Configure an HTTPS `[dashboard] public_url`; reverse-proxy path prefixes are preserved. Without a public URL, use the dashboard page.

Completion notices show duration, detected speakers and filenames. Telegram, Discord and Slack can attach Markdown using the original bot and thread; LINE points to the available attachment. WebChat persists the notice in the original conversation, visible on refresh. Channels without a verifiable original transport target report notification failure rather than switching bots. Notification retries use backoff, at most five attempts; outputs remain available on the dashboard.

## Configuration

Admins select the provider/model in **Settings → Voice → Meeting transcription**. File limits, staging quota and concurrency are configured in `config.toml`:

```toml
[transcription]
engine = "gemini" # gemini | whisper; google_stt is not implemented
# model = "gemini/<audio-and-structured-model-in-your-registry>"
max_bytes = 4294967296 # 1 through 4294967296; default 4 GiB
staging_quota_bytes = 10737418240 # default 10 GiB, shared by jobs and uploads
media_memory_limit_bytes = 2147483648 # 2 GiB
max_concurrent_jobs = 1 # 1 or 2
```

Gemini uses the configured audio/structured model or resolves one from ModelRegistry. Whisper uses the existing OpenAI credentials and does not identify speakers. Gemini transcription can be inaccurate and speaker labels may change between chunks. `google_stt` fails explicitly; there is no fallback to another vendor. Employees cannot override the administrator's provider selection.

Install **ffmpeg and ffprobe** for video extraction and roughly 30-minute audio chunks, up to 96 chunks/48 hours. Without ffmpeg, direct audio must fit provider limits and ffprobe must verify a duration of at most 30 minutes; unknown or longer recordings are refused. Every result passes through OpenCC `s2twp` Taiwan phrase conversion; this does not translate other languages.

Quota admission reserves the source snapshot and encoded-audio allowance; direct uploads also reserve their uploaded source. Actual admission therefore needs more space than the original file alone. On termination, uploaded sources and temporary audio are removed; transcript outputs remain. Original files submitted from ordinary employee attachments are retained. Gateway restart requeues unfinished jobs from scratch, using fixed job output names. Provider work may be repeated and billed again; recorded costs accumulate and interrupted in-flight usage may be unmeasured.

Only the gateway lock holder runs jobs. Employee pauses stop admission and later chunks. Changes to permission, egress or redaction policy stop older jobs until resubmitted. Dashboard/link uploads fail closed when policy requires an additional approval; interactive web-upload approvals are not implemented in this version.

## Operator CLI and privacy

```sh
duduclaw meeting transcribe ./meeting.mp4 --out ./transcripts
```

The local operator CLI remains synchronous and refuses AI employee sessions. The shared Bash operator-command guard also blocks employee and unverified callers; employees use the meeting MCP tools. `write_wiki: true` is unsupported; review the result and use existing wiki tools separately.

Audio leaves the host for the selected provider. Content sent to the free Gemini API tier may be used to improve products; check the terms for the account and the recording's confidentiality requirements. Configured redaction masks transcript text and source names before storage, failing closed on errors. It cannot remove sensitive speech from audio already sent to the provider. Employee redaction can narrow global policy, never turn it off.

Real provider quality, long-video browser conversion, platform-specific ffmpeg, real channel attachments and large network resume need separate acceptance. Mock tests do not establish those live outcomes.

## Security and resource limits

Reads and downloads require Viewer; mutations require Operator. Pending uploads reserve their size plus 64 KiB, with at most two uploads and half the shared quota per owner and per employee. Cancellation and expiry immediately release reservations. Jobs bind approvals and egress to a gateway-owned SHA-256/size snapshot and verify the worker copy. Retained output files count by actual bytes. AI status responses contain metadata and attachment paths only; dashboard previews are separate and completion notices omit transcript excerpts.

Jobs stop at 200,000 segments, 32 MiB of text/speaker bytes or 256 MiB of estimated/actual output. Upload/download slots are separate; each owner has one in-flight body and upload windows require 32 KiB/s over 30 seconds. Public routes apply IP/global admission before token hashing or database access, then per-link limits after validation. Masked audits exclude tokens, keys, transcript text and sensitive paths. Whisper is charged by the locally measured chunk duration. Gemini audio usage below 16 tokens per local second falls back to an explicitly marked local estimate. Meeting tool-call audits retain only an attachment label and a validated job-id prefix; capability URLs and result text are excluded.

Unix media children limit CPU time and open descriptors (256). Linux applies an address-space limit; macOS attempts AS/DATA limits. On both platforms an RSS watchdog runs every 500 ms, including when rlimits are unavailable. Exceeding `media_memory_limit_bytes` (default 2 GiB) terminates the process group and fails the job; RSS inspection failures also stop the job.
