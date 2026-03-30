# Security Patch Release SOP

> Defines the process for releasing security patches across commercial and open-source editions.

## Patch Timeline

| Severity | Commercial (Pro/Enterprise/OEM) | Open Source (Community) |
|----------|-------------------------------|------------------------|
| Critical (CVSS 9.0+) | Immediate (< 4hr) | 7 days |
| High (CVSS 7.0-8.9) | Same day (< 24hr) | 14 days |
| Medium (CVSS 4.0-6.9) | Within 3 days | 30 days |
| Low (CVSS < 4.0) | Next release cycle | Next release cycle |

## Process

### 1. Discovery & Triage (< 1hr)

- [ ] Reproduce the vulnerability
- [ ] Assign CVSS score using CVSS 3.1 calculator
- [ ] Determine affected versions
- [ ] Assign severity: Critical / High / Medium / Low
- [ ] Create private tracking issue (not public GitHub)

### 2. Fix Development (varies by severity)

- [ ] Develop fix on private branch
- [ ] Write regression test
- [ ] Code review by maintainer
- [ ] Run full test suite
- [ ] Verify fix resolves the vulnerability
- [ ] Verify no regressions introduced

### 3. Commercial Release

- [ ] Build release binary with fix
- [ ] Update `duduclaw update` channel
- [ ] Notify affected commercial customers via email
  - Subject: `[DuDuClaw Security] {severity} — {brief description}`
  - Include: affected versions, fix version, upgrade instructions
- [ ] Update commercial changelog

### 4. Embargo Period (for Community edition)

During the embargo period:
- Do NOT push the fix to the public repository
- Do NOT discuss the vulnerability publicly
- Do NOT create public GitHub issues about it
- Commercial customers are bound by NDA not to disclose

### 5. Open Source Release (after embargo)

- [ ] Push fix to public repository
- [ ] Tag new release version
- [ ] Update Homebrew formula
- [ ] Publish GitHub Security Advisory (GHSA)
- [ ] Post advisory on Discord #announcements
- [ ] Post on X/Twitter if High or Critical

### 6. Post-Incident

- [ ] Update this SOP if gaps were found
- [ ] Review if similar vulnerabilities exist elsewhere in codebase
- [ ] Update security scanning rules if applicable

## Communication Templates

### Commercial Customer Notification

```
Subject: [DuDuClaw Security] {SEVERITY} — {title}

Dear {customer_name},

A {severity} security vulnerability has been identified in DuDuClaw
versions {affected_versions}.

Impact: {brief description of impact}
CVSS Score: {score}

A fix is available in version {fix_version}. Please update immediately:

  duduclaw update

If you cannot update immediately, the following workaround is available:
{workaround or "No workaround available — update required."}

This information is confidential until the public disclosure date
({public_date}).

Questions? Reply to this email or contact us via LINE OA.

— DuDuClaw Security Team
```

### Public Advisory (after embargo)

```
## DuDuClaw Security Advisory — {GHSA-ID}

**Severity**: {severity}
**CVSS**: {score}
**Affected versions**: {versions}
**Fixed in**: {fix_version}

### Description
{description}

### Impact
{impact}

### Remediation
Update to version {fix_version}:
  brew upgrade duduclaw
  # or
  duduclaw update

### Timeline
- {date}: Vulnerability discovered
- {date}: Fix released to commercial customers
- {date}: Public disclosure (this advisory)

### Credit
{reporter if applicable}
```

## Responsible Disclosure Policy

If you discover a security vulnerability in DuDuClaw:

1. **Do NOT** open a public GitHub issue
2. Email: security@duduclaw.dev (or the designated contact)
3. Include: description, reproduction steps, affected version, impact assessment
4. We will acknowledge within 24 hours
5. We aim to release a fix within the timelines above
6. We will credit reporters (unless they prefer anonymity)
