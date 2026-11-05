# Meeting transcripts

[繁體中文](zh-TW/73-meeting-transcripts.md) · [日本語](ja-JP/73-meeting-transcripts.md)

Convert completed Meet, Zoom or Teams recordings into Taiwan Traditional Chinese Markdown and JSON, with timestamps and optional speaker labels. Upload a local file or, in Enterprise, select a file from your personal Google Drive connection. This does not join or record live meetings.

## Upload and follow progress

Open **Meeting transcripts**, choose an employee you may use, and select audio or video. Non-admin accounts are restricted to directly bound, selectable employees with Operator/Owner access; main/default and administrator-only machine employees remain unavailable. Files use 8 MiB chunks with a server-verified SHA-256 per chunk. Select the same file after an interruption to resume. Incomplete uploads expire after two hours without a new chunk.

When WebCodecs supports the recording, the browser attempts to extract MP4/MOV AAC-LC audio and encode mono AAC at about 48 kbps. WebM, unsupported codecs or any conversion error fall back to the original file. The page reports the selected path. The demuxer is an exact-pinned mp4box.js release under BSD-3-Clause. Browser conversion is best effort.

The job list shows queued, preparing, transcribing with chunk progress, done, failed or cancelled. Download Markdown and JSON after completion. Cancellation prevents subsequent chunks; an already submitted provider request finishes and records its available usage. Cancellation cannot undo provider charges.

## Import from Google Drive and Meet (Enterprise)

In **My connections**, enable meeting-recording access for your own Google connection and accept the separate consent step. Initial personal connection does not request Drive or Meet access. The extra scopes are:

- `https://www.googleapis.com/auth/drive.file`: access files you select or share with the app through Google Picker. Google also permits creation and modification within this scope; this feature uses read requests only.
- `https://www.googleapis.com/auth/meetings.space.readonly`: list recent conferences and their recording references.

The gateway records local consent separately for basic services, meeting recordings and employee Drive tools, and binds it to the sealed credential and its authority record. A capability needs both the provider scope and the matching local purpose consent. Extra `drive.readonly` or `drive` scopes returned during meeting consent cannot enable employee Drive tools. Incremental consent preserves earlier local purposes and must retain every previously held provider scope; publication also checks the original credential revision. Missing scopes or a concurrent credential change leave the old credential unchanged. Records without local purpose consent require reconnection. Reconnecting basic services resets local purposes to basic services, so recording access and employee Drive tools must then be enabled separately again.

Open **Meeting transcripts**, select the employee, and choose **Choose from Google Drive**. Recent Meet recordings come from the Meet API; select a recording in Google Picker to grant file access. You can also search audio/video files already shared with the app, or open Google Picker to choose another recording. Review the selected file and employee before confirming import. The audio is sent to the administrator's configured transcription provider under the same privacy and outgoing-data checks as uploads.

Every import requires a server-issued confirmation receipt. It proves that a dashboard user authenticated by JWT explicitly registered and confirmed this exact Drive file ID for this exact receiving employee within the last five minutes. Employee/MCP tokens cannot register it. The receipt is single use and bound to that user, employee and file ID; a new confirmation for the same user and employee replaces the previous receipt. The dashboard registers the file only when the user confirms the import. Missing, expired, reused or mismatched receipts are rejected. An admitted import attempt consumes the receipt even if later checks fail, so retry with a new confirmation. The server additionally checks Drive metadata for `isAppAuthorized=true` under that user's Google connection. Neither the receipt nor this Google flag proves a click in Google Picker: a dashboard caller can explicitly confirm a previously app-authorized file by its known ID.

The dashboard-only `POST /api/meeting-drive/selection` requires `agent_id`, `file_id` and `confirm: true`, and returns `selection_receipt` with `expires_in`. Registration is limited to 20 requests per user per minute, with at most eight unexpired receipts per user. `POST /api/meeting-drive/import` must submit that receipt.

`google_status` reads account, status and purpose-filtered scope metadata only: it does not unseal credentials, refresh tokens or return a bearer. The token endpoint accepts an explicit tool allowlist and requires both the matching local purpose and provider scope for personal credentials; status requests and unknown tools cannot obtain a bearer.

Meet recordings usually live in the organizer's **Meet Recordings** folder (the name may be localized). A conference reference does not guarantee access to its Drive file. The recent list reads up to ten conferences per page and five recordings per conference; use the next page when available. A limited-results notice means the list may omit recordings; use Google Picker to find the desired file.

The gateway downloads the original file into the existing upload storage in bounded 8 MiB chunks. Upload size, staging quota, media-type validation and duration limits still apply; browser audio conversion is not used for Drive imports. Downloads have a 30-minute total limit and require an average of 32 KiB/s per 30-second window. Leaving the page or breaking the browser HTTP connection does not guarantee the server stops importing. Check the job list afterwards and cancel the job there if needed. A server refusal or cancelled import task triggers temporary-file cleanup; after a gateway crash, the ordinary two-hour upload idle expiry reclaims incomplete imports.

The feature adds no employee tool for browsing or fetching your Drive. It requires your dashboard session, current personal connection and explicit file selection. Disconnecting, a detected revoked permission, or a changed local access policy stops further access; it does not recall copies already imported or completed transcripts. Existing broader Google grants are not automatically revoked when upgrading. See [personal Google connection setup](../guides/google-personal-connections.md) for Picker configuration and revocation details.

The local Drive source record contains the selecting account, Google email, file ID and version. Cancellation, failure and upload expiry remove this record in the same database transaction as the corresponding state change. Other source records, including successful imports, have a 30-day retention period from `imported_at`, then are removed by the existing cleanup sweep every 60 seconds while the gateway is running; after downtime, cleanup occurs on the next sweep. Transcript files and audit logs have separate retention rules and are not deleted by this source-metadata cleanup.

`user.google_connect` audits include the consent purpose, a digest of the returned scope set and unexpected scopes. Drive/Meet list reads and early refusals are also audited. These events omit tokens and file contents; import identifiers are hashed.

Real Google OAuth/Picker behavior, Drive/Meet access and provider transcription require separate acceptance. Mock results and source inspection do not establish those outcomes.

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
engine = "gemini" # gemini | whisper | google_stt
# model = "gemini/<audio-and-structured-model-in-your-registry>"
max_bytes = 4294967296 # 1 through 4294967296; default 4 GiB
staging_quota_bytes = 10737418240 # default 10 GiB, shared by jobs and uploads
media_memory_limit_bytes = 2147483648 # 2 GiB
max_concurrent_jobs = 1 # 1 or 2
```

Gemini uses the configured audio/structured model or resolves one from ModelRegistry. Whisper uses the existing OpenAI credentials and does not identify speakers. Gemini transcription can be inaccurate and speaker labels may change between chunks.  Employees cannot override the administrator's provider selection.

Install **ffmpeg and ffprobe** for video extraction and roughly 30-minute audio chunks, up to 96 chunks/48 hours. Without ffmpeg, direct audio must fit provider limits and ffprobe must verify a duration of at most 30 minutes; unknown or longer recordings are refused. Every result passes through OpenCC `s2twp` Taiwan phrase conversion; this does not translate other languages.

Quota admission reserves the source snapshot and encoded-audio allowance; direct uploads also reserve their uploaded source. Actual admission therefore needs more space than the original file alone. On termination, uploaded sources and temporary audio are removed; transcript outputs remain. Original files submitted from ordinary employee attachments are retained. Gateway restart requeues unfinished jobs from scratch, using fixed job output names. Provider work may be repeated and billed again; recorded costs accumulate and interrupted in-flight usage may be unmeasured.

Only the gateway lock holder runs jobs. Employee pauses stop admission and later chunks. Changes to permission, egress or redaction policy stop older jobs until resubmitted. Dashboard/link uploads fail closed when policy requires an additional approval; interactive web-upload approvals are not implemented in this version.

### Google Cloud Speech-to-Text v2

`google_stt` uses `chirp_2` in `asia-southeast1` with default `cmn-Hant-TW`. [Google's Chirp 2 documentation](https://docs.cloud.google.com/speech-to-text/docs/models/chirp-2) lists Taiwan Mandarin and batch recognition, but no diarization. This engine currently accepts only `cmn-Hant-TW` and `chirp_2`, and rejects diarization rather than silently ignoring it. Regions are limited to `asia-southeast1`, `us-central1` and `europe-west4`. Direct audio chunks are capped locally at 25 MiB; ffmpeg-generated 30-minute AAC chunks fit this ceiling. Word offsets map into transcript segments; speaker labels remain absent. The operator-configured language takes precedence over per-job hints.

1. Enable Cloud Speech-to-Text and Cloud Storage in a billing-enabled Google Cloud project. Create a private bucket in a compatible location.
2. Create a dedicated service account with Speech Client (`roles/speech.client`) and object create/read/delete permissions limited to this bucket. Grant the project's Speech service agent read access to the bucket when required, especially across projects. Workspace OAuth and Gemini API keys are not used.
3. In **Settings → Voice → Meeting transcription**, select Google Cloud Speech-to-Text; enter project ID, location, bucket name (without `gs://`), language and `chirp_2`. Paste the service-account JSON into the write-only credential field. The gateway encrypts it with the existing machine-key credential handling; blank preserves the installed key. The key is never returned to the dashboard or employees. Use **Remove saved key** to clear it. The dashboard calls gateway-owned `transcription_credentials.set` (`google_stt_key`) and `.clear` RPCs; both require a live Admin identity and run before extension dispatch. Generic `system.update_config` refuses credential fields. The raw editor accepts only an unchanged mask or removal for this credential, never new plaintext or caller-supplied ciphertext, and validates transcription settings. This validates the key format locally, not its live IAM permissions.
4. Submit a short recording first, then verify a long recording and your billing/IAM settings. No live Google acceptance has been performed for this implementation.

```toml
[transcription.google_stt]
project_id = "your-project-id"
location = "asia-southeast1"
bucket = "your-private-meeting-bucket"
language = "cmn-Hant-TW"
model = "chirp_2"
diarization = false
```

[BatchRecognize requires GCS audio input](https://docs.cloud.google.com/speech-to-text/docs/batch-recognize); inline output does not remove that requirement. Before uploading each chunk under a random temporary object name, the gateway durably records its bucket, object, generation (unknown until the upload returns), and cleanup state under its home directory with file locking in `<home>/google_stt_cleanup/<uuid>.json`; records contain no audio or credentials. Active uploads and recognition hold an OS lock that reconciliation skips. Known generations are deleted explicitly; if an upload response was lost, the record remains through a 900-second settling window even after an early successful delete or HTTP 404, then deletion is checked again. Transient failures get at most three attempts per pass, with 1- and 2-second backoff; pending entries survive crashes and are retried at startup and every 60 seconds until deletion succeeds or returns HTTP 404. Cleanup is pinned to the currently configured bucket: mismatched records stay untouched and produce a count-only warning. Finish cleanup before changing buckets, or restore the previous bucket setting to retry its records; changing engines alone does not stop cleanup. Keep valid credentials and bucket deletion permissions until cleanup finishes. On Unix the journal directory is 0700 and journal/lock files are 0600; symbolic links and non-regular files are refused. A bucket lifecycle expiration rule remains a backstop for unavailable credentials or prolonged failures; in-flight recognition and cloud charges cannot be undone by cancellation. Polling is bounded, and restart may repeat paid recognition. Estimates use the standard V2 list rate of $0.016/minute, rounded up to seconds, without volume discounts, storage, network or other cloud charges; the ledger marks them estimated, not billing receipts. See [Google pricing](https://cloud.google.com/speech-to-text/pricing). Provider quality, regional access, real IAM, cleanup and charges remain unverified.


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
