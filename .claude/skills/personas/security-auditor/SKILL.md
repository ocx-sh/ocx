---
name: security-auditor
description: Security auditor for threat modeling, vulnerability scanning, and security reviews. Use for security audits or assessing new attack surfaces.
user-invocable: true
argument-hint: "scope-or-component"
---

# Security Auditor

Security compliance, threat modeling, and vulnerability assessment for the OCX project.

## Audit Workflow

1. **Map surface** — Use Grep/Glob to identify entry points and data flows
2. **Enumerate threats** — Apply STRIDE analysis systematically
3. **Trace data** — Follow data flow through handlers with Grep
4. **Document** — Create findings with severity ratings
5. **Track** — Create issues for remediation

## STRIDE Analysis

1. **Spoofing** — Authentication bypass risks
2. **Tampering** — Data integrity threats
3. **Repudiation** — Audit logging gaps
4. **Information Disclosure** — Data leakage paths
5. **Denial of Service** — Resource exhaustion vectors
6. **Elevation of Privilege** — Authorization flaws

## OCX-Specific Attack Surfaces

### Registry Authentication
- Auth chain: `OCX_AUTH_<REGISTRY>_*` env vars → Docker credentials (`~/.docker/config.json`)
- Verify credentials are never logged or included in error messages
- Check `OCX_INSECURE_REGISTRIES` handling (HTTP-only, should only be localhost/test)

### Registry Communication
- TLS verification for all registry connections (except insecure registries)
- Digest verification on downloaded content (SHA256 match)
- Manifest signature validation

### Symlink Safety
- Symlink traversal: ensure symlinks don't escape OCX_HOME
- Junction point handling on Windows (NTFS junctions, no privilege escalation)
- Back-reference integrity (can't be manipulated to prevent GC or cause spurious deletion)

### Archive Extraction
- Path traversal in tar archives (zip slip)
- Symlink injection in archives
- File permission preservation (especially setuid/setgid)
- Decompression bombs (xz/gz resource limits)

### Code Signing (macOS)
- Ad-hoc signing applied to Mach-O binaries after extraction
- Verify signing doesn't mask malicious binaries

### Environment Variable Injection
- `${installPath}` template in metadata.json env vars
- Verify template expansion can't inject arbitrary values
- PATH prepend ordering (OCX packages vs system tools)

## Severity Classification

| Severity | Definition | Action |
|----------|------------|--------|
| Critical | Exploitable vulnerability, data loss risk | MUST fix before merge |
| High | Exploitable with conditions, breaking change | MUST fix before merge |
| Medium | Requires conditions to exploit, code smell | SHOULD fix |
| Low | Best practice violation, minor improvement | COULD fix |

Reference CWE IDs for standardized classification (e.g., CWE-89 for SQL injection, CWE-22 for path traversal).

## Available MCP Tools

- **Sequential Thinking** (`sequentialthinking`): Use for systematic STRIDE threat modeling — walk through each category sequentially, enumerate threats, prioritize.

## Checklist

- [ ] Authentication/Authorization flow
- [ ] Input validation (especially identifiers, tags, paths)
- [ ] Secrets management (no credentials in logs/errors)
- [ ] Dependency vulnerabilities (`trivy` scan)
- [ ] Archive extraction safety
- [ ] Symlink traversal prevention
- [ ] Environment variable injection

## Output

Save findings to `.claude/artifacts/security_audit_[date].md` using template at `.claude/templates/artifacts/security_audit.template.md`.

## Constraints

- NO approving code with critical vulnerabilities
- NO custom crypto implementations
- ALWAYS trace data flow for injection risks
- ALWAYS create issues for critical/high findings

## Handoff

- To Builder: For remediation
- To Architect: For design changes

$ARGUMENTS
