# Security Policy

## Supported Versions

| Version | Supported          | Patch Timing |
|---------|--------------------|-------------|
| Latest release (Commercial) | :white_check_mark: | Immediate |
| Latest release (Community)  | :white_check_mark: | Up to 30 days delay |
| Previous minor              | :white_check_mark: | Critical only |
| Older versions              | :x:                | Not supported |

## Reporting a Vulnerability

**Do NOT open a public GitHub issue for security vulnerabilities.**

### Preferred: GitHub Private Vulnerability Reporting

1. Go to the [Security Advisories](https://github.com/anthropics/DuDuClaw/security/advisories) page
2. Click **"Report a vulnerability"**
3. Fill in the details and submit

### Alternative: Email

Send an email to **security@duduclaw.dev** with:

- **Subject**: `[SECURITY] Brief description`
- **Description**: Detailed description of the vulnerability
- **Impact**: What can an attacker achieve?
- **Steps to reproduce**: Minimal steps to trigger the issue
- **Affected versions**: Which versions are affected?
- **Suggested fix**: If you have one (optional)

### Response Timeline

| Severity | Acknowledgment | Fix Target | Disclosure |
|----------|---------------|------------|------------|
| Critical | 24 hours | 72 hours | After fix released |
| High     | 48 hours | 7 days | After fix released |
| Medium   | 7 days | 30 days | After fix released |
| Low      | 14 days | Next release | After fix released |

### What to Expect

1. **Acknowledgment**: We will confirm receipt within the timeline above
2. **Assessment**: We will evaluate the severity and impact
3. **Fix**: We will develop and test a patch
4. **Release**: Commercial editions receive patches immediately; Community edition follows per the [Security Patch SOP](docs/security-patch-sop.md)
5. **Credit**: We will credit you in the advisory (unless you prefer anonymity)

## Scope

The following are in scope for security reports:

- **DuDuClaw core** (all 12 Rust crates)
- **Web Dashboard** (React frontend)
- **Python bridge** (`python/duduclaw/`)
- **Container sandbox** escape or bypass
- **SOUL Guard** bypass (SHA-256 drift detection)
- **Input Guard** bypass (prompt injection scanner)
- **Credential Proxy** key leakage
- **AES-256 encryption** weaknesses
- **Ed25519 auth** bypass
- **RBAC** privilege escalation
- **CONTRACT.toml** validation bypass
- **Browser automation** unauthorized escalation (L1-L5)

### Out of Scope

- Issues in third-party dependencies (report upstream, but notify us)
- Denial of service via resource exhaustion (unless trivially exploitable)
- Social engineering attacks
- Issues requiring physical access to the machine
- Vulnerabilities in the Claude API or Claude Code SDK itself

## Security Architecture

DuDuClaw implements defense in depth:

- **Layer 1**: Input Guard — prompt injection detection (6 rule categories, risk score 0-100)
- **Layer 2**: RBAC Engine — 7 permission types, per-agent role enforcement
- **Layer 3**: SOUL Guard — SHA-256 drift detection with 10 versioned backups
- **Layer 4**: Credential Proxy — per-agent key isolation with AES-256-GCM encryption
- **Layer 5**: Container Sandbox — Docker/Apple Container with `--network=none`, tmpfs, read-only rootfs
- **Layer 6**: CONTRACT.toml — behavioral boundary enforcement with runtime validation
- **Layer 7**: Audit Log — append-only JSONL security event trail

For full architecture details, see [docs/CLAUDE.md](docs/CLAUDE.md).

## Disclosure Policy

We follow [coordinated vulnerability disclosure](https://en.wikipedia.org/wiki/Coordinated_vulnerability_disclosure). We ask that you:

- Give us reasonable time to fix the issue before public disclosure
- Do not exploit the vulnerability beyond what is necessary to demonstrate it
- Do not access or modify other users' data

We commit to:

- Not pursuing legal action against good-faith security researchers
- Crediting researchers in security advisories
- Keeping researchers informed of fix progress
