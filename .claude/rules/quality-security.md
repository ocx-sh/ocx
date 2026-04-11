# Security Standards

Deep-dive reference for security reviews. See Core Principle 3 ("Keep It Safe") in CLAUDE.md for the essentials.

## Security Checklist

- [ ] No hardcoded secrets or credentials
- [ ] All user input is validated and sanitized
- [ ] SQL queries use parameterized statements
- [ ] Authentication and authorization are properly implemented
- [ ] Sensitive data is encrypted at rest and in transit
- [ ] Error messages don't expose internal details
- [ ] Dependencies are up to date and vulnerability-free

## OWASP Top 10 2021

| Category | Check For |
|----------|-----------|
| Broken Access Control | Missing authorization checks |
| Cryptographic Failures | Unencrypted sensitive data |
| Injection | SQL, Command, XSS vulnerabilities |
| Insecure Design | Missing threat modeling |
| Security Misconfiguration | Default credentials, debug enabled |
| Vulnerable Components | Outdated/CVE-affected packages |
| Auth Failures | Weak passwords, session issues |
| Integrity Failures | Unsigned updates, untrusted deserialization |
| Logging Failures | Missing audit trails |
| SSRF | Unvalidated URLs in server requests |

## Severity Classification

| Severity | Definition | Action |
|----------|------------|--------|
| Critical | Exploitable vulnerability, data loss risk, high impact | MUST fix before merge |
| High | Exploitable vulnerability, breaking change, moderate impact, major bug | MUST fix before merge |
| Medium | Requires conditions to exploit, performance issue, code smell | SHOULD fix, can negotiate |
| Low | Best practice violation, style, minor improvement | COULD fix, optional |

## CWE References

When reporting findings, reference CWE (Common Weakness Enumeration) IDs for standardized vulnerability classification. Example: `CWE-89` for SQL Injection, `CWE-798` for hardcoded credentials.

## Dependency Safety

- Warn about deprecated or vulnerable dependencies
- Audit new dependencies before adding
- Keep dependencies updated
- Use automated scanning (Trivy, Snyk, Dependabot)

## Output Guidelines

- Never expose actual secrets in analysis output
- Provide specific file locations and line numbers
- Include concrete remediation steps
- Check both code AND configuration files

## OCX-Specific Attack Surfaces

These are the recurring attack surfaces in the OCX codebase. Use them as the STRIDE scoping checklist for any OCX audit.

### Registry Authentication
- Auth chain: `OCX_AUTH_<REGISTRY>_*` env vars → Docker credentials (`~/.docker/config.json`)
- Credentials must never be logged or included in error messages
- `OCX_INSECURE_REGISTRIES` (HTTP-only) should only be used for localhost/test registries

### Registry Communication
- TLS verification for all registry connections (except insecure registries)
- Digest verification on downloaded content (SHA-256 match)
- Manifest signature validation

### Symlink Safety
- Install symlinks must not escape `OCX_HOME` via traversal
- Windows junction point handling (NTFS junctions, no privilege escalation)
- Back-reference integrity — cannot be manipulated to prevent GC or cause spurious deletion

### Archive Extraction
- Path traversal in tar archives (zip slip)
- Symlink injection in archives
- File permission preservation (especially setuid/setgid)
- Decompression bombs (xz/gz resource limits)

### Code Signing (macOS)
- Ad-hoc signing applied to Mach-O binaries after extraction
- Signing must not mask malicious binaries

### Environment Variable Injection
- `${installPath}` template expansion in `metadata.json` env vars
- PATH prepend ordering (OCX packages vs system tools)

## OCX Audit Checklist

- [ ] Authentication/authorization flow
- [ ] Input validation (identifiers, tags, paths)
- [ ] Secrets management (no credentials in logs/errors)
- [ ] Dependency vulnerabilities (`trivy` scan)
- [ ] Archive extraction safety
- [ ] Symlink traversal prevention
- [ ] Environment variable injection
