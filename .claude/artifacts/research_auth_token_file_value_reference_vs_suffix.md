# Research: Secret from file/command — value reference (V) vs companion suffix (S)

## Metadata

**Date:** 2026-11-23
**Domain:** cli, security
**Triggered by:** decision on how `PREFIX_<HOST>_TOKEN` variables load a secret from a file (incl. Windows drive/UNC paths) and possibly from command output
**Expires:** 2027-05-23 (re-verify)

Method note: every page below was read through a fetch tool that returns a model-written summary of the page, not raw text. Quotes marked "(summary)" are the tool's rendering. Items marked "(snippet)" come from a search-result snippet only. Nothing found was older than ~18 months except where flagged "OLD".

Terms. S = companion variable/option (`X_FILE`, `X_COMMAND`) beside `X`. V = reference inside the value of `X`. Mix = V for file/env, S for command.

## Direct Answer

No verdict. Evidence balance:

**S (suffix)**
- For: dominant in container-image ecosystems (Docker official images, Gitea, Grafana, MinIO, MySQL, Postgres) and in backup/crypto tools (restic, borg, rclone, sops). The value of `X` is never interpreted, so a literal secret is always safe. A command source is a separate name, so "may hold a secret" and "may run a program" stay separate switches. Both-set conflict has a precedent answer (Docker `file_env` aborts; borg/restic/rclone define precedence).
- Against: variable count doubles (or triples with `_COMMAND`); naming drift (`_FILE` vs `__FILE`, documented in Grafana threads); "both set" needs a rule and every tool picks a different one; tools forget the suffix on some variables (Postgres supports it for 4 variables only); with host-keyed names the suffix set is `PREFIX_<HOST>_TOKEN_FILE` — name length and host-mangling rules apply twice. Docker `file_env` can also only be applied to names a program knows; no source found arguing S for a dynamic name space like host-keyed variables.

**V (value reference)**
- For: one name per secret; extends to new sources without new names; works for dynamic/host-keyed names; precedent in cosign (`env://`), AWS CLI (`file://`), OpenSSL (`file:`/`env:`/`fd:`), 1Password (`op://`), Grafana (`$__file{}`), Spring (`configtree:`), Kong (`{vault://}`), Vector (`SECRET[...]`), Telegraf (`@{store:key}`).
- Against: literal values that start with the prefix become unreadable (AWS needs a global switch to disable; curl needs a separate `--data-raw`; HTTPie needs `\` escape); a later new scheme changes the meaning of existing values; attacker-controlled values gain file read where a value is interpreted (curl `@file` in Burp/Chrome 2023; Ansible CVE-2017-7481 for templating); Windows `C:\x` and `\\server\share` collide with scheme and URI grammars; a secret variable that holds a path or a `file://` string is not a secret to scanners/redaction (no source found, see `negative:`).

**Mix (V for file/env, S for command)**
- For: matches how kubeconfig/Flux, git, AWS `credential_process`, msmtp, mbsync, aerc, rclone, borg, restic, sops keep a command in a different key from a literal/value key; the security advice in kubeconfig/Flux/AWS is about the command key being opt-in. Grafana is the closest precedent for mixing (`$__file{}` in ini and `__FILE` env suffix both exist).
- Against: two grammars to document; no program found that does exactly "V for file/env + S for command" for the same secret. Grafana's two mechanisms are for different layers (ini vs env), not a deliberate split by source type.

## Comparison table

| Program | S or V | Exact grammar | Bare value means | Command source (spelling) | Windows notes |
|---|---|---|---|---|---|
| cosign | V | `[scheme://]rest`; `env://VAR`, `http(s)://`, KMS `awskms://`, `gcpkms://`, `azurekms://`, `hashivault://`, `k8s://ns/name` | local file path (`os.ReadFile(filepath.Clean(ref))`) | none seen (external plugin binary for OVH KMS) | split on first `://`, so `C:\x` has no `://` and is a bare path; `file://` is NOT a case in `LoadFileOrURL` (summary of source) |
| AWS CLI v1 docs | V | `file://path`, `fileb://path`, `http(s)://` | literal string | none | doc shows `file://C:\temp\filter.json`; paths are raw, `~/ ./ ../` expanded; docs call it a "URL" |
| OpenSSL | V | `pass:` `env:` `file:` `fd:` `stdin` | not allowed (must have a source word) | none | `fd:` not supported on Windows; `file:` reads first line only |
| curl | V (in `@`) | `-d @file`, `-H @file`, `-F name=@file`; `--data-raw`, `--form-string` disable | literal | none | not examined |
| HTTPie | V (in `=@`) | `name=@file`, `X:@file`; backslash escapes | literal | none | not examined |
| Grafana | both | `$__env{X}`, `$__file{path}`, `$__vault{}` in ini; env `GF_<SEC>_<KEY>__FILE` | literal | none (vault is Enterprise) | not examined; file expansion trims whitespace both ends |
| 1Password | V | `op://vault/item/[section/]field[?attribute=..]` resolved by `op run`/`op inject` | literal (page silent) | `op` itself is the resolver | not examined |
| Kong | V | `{vault://backend/name/field}` | not confirmed | n/a | not examined |
| Vector | V | `SECRET[backend.name]`; backend `type: exec|file|directory|aws_secrets_manager` | literal | `exec` backend defined in config with `command: [..]`, not in the reference | not examined |
| Telegraf | V | `@{storeid:secretname}`; stores `os`, `jose` | literal | no `exec` store on the page read | not examined |
| Home Assistant | V (YAML tag) | `!secret id` | literal | none | not examined |
| Spring Boot | V (import prefix) | `spring.config.import=optional:configtree:/dir/` | literal | none | page has no Windows notes |
| systemd | S-like (file-first) | `LoadCredential=`, `SetCredential=`, `%d` -> `$CREDENTIALS_DIRECTORY` | n/a | n/a | n/a |
| Docker BuildKit | V-in-option | `--secret id=x,src=path` / `id=x,env=VAR` | bare `id=x` auto-detects (env var if matches) | none | not examined |
| Docker official images | S | `X_FILE`; Postgres: only 4 vars | n/a | none | n/a |
| Gitea | S | `GITEA__SECTION__KEY__FILE` (snippet) | n/a | none | n/a |
| MinIO | S | `MINIO_ROOT_PASSWORD_FILE` (snippet) | n/a | none | n/a |
| PostgreSQL client | S | `PGPASSFILE`; `PGPASSWORD` discouraged | n/a | none (2013 askpass/command proposals, outcome unconfirmed) | n/a |
| restic | S | `RESTIC_PASSWORD`, `_FILE`, `_COMMAND` | n/a | `RESTIC_PASSWORD_COMMAND` | not examined |
| borg | S | `BORG_PASSPHRASE`, `_FD`, `BORG_PASSCOMMAND`, `BORG_NEW_PASSPHRASE` | n/a | `BORG_PASSCOMMAND` (no shell) | not examined |
| rclone | S | `RCLONE_CONFIG_PASS`, `--password-command` / `RCLONE_PASSWORD_COMMAND` | n/a | `--password-command` | not examined |
| Ansible | S + mode bit | `--vault-password-file`; executable file is run (script header comment, summary) | n/a | executable file | not examined |
| git | separate key | `credential.helper = !snippet | /abs/path | name` | n/a | leading `!` = shell | not examined |
| AWS `credential_process` | separate key | `credential_process = cmd args` | n/a | own config key | `"C:\Path\To\x.cmd" args` quoting example |
| msmtp / mbsync / aerc | separate key | `passwordeval` / `PassCmd` / `source-cred-cmd` | `password` is literal | own key | n/a |
| kubeconfig | separate key | `users[].user.exec.command` | n/a | own key | n/a |
| sops/age | S (not verified) | `SOPS_AGE_KEY(_FILE|_CMD)` | n/a | not verified (README had no mention) | n/a |
| GitLab CI | S-like | file-type variable: value = path to temp file | n/a | n/a | n/a |

## Key findings

### 1. V users: grammar and bare value

1. cosign: `LoadFileOrURL` splits on the first `://`; `env://NAME` reads an env var, `http(s)://` is fetched, any other scheme returns `UnrecognizedSchemeError`, no `://` means a path read with `os.ReadFile(filepath.Clean(ref))`. Source (summary of file on `main`, re-verify): <https://raw.githubusercontent.com/sigstore/cosign/main/pkg/blob/load.go>. Tests cover bare paths and `invalid://url`, not `file://`: <https://raw.githubusercontent.com/sigstore/cosign/main/pkg/blob/load_test.go>. The KMS/k8s forms are documented at <https://docs.sigstore.dev/cosign/key_management/overview/>; that page does not list `env://` nor say what a bare value means. So "cosign supports `file://`" was not found; it was found to be rejected in the loader.
2. AWS CLI: `file://` and `fileb://` take a path after the first two slashes: "The first two slash '/' characters are part of the specification. If the required path begins with a '/', the result is three slash characters". Windows example `file://C:\temp\filter.json`; `~/` expands to `%USERPROFILE%`. Behaviour is "disabled automatically for parameters that already expect a URL" and by the `cli_follow_urlparam` setting. No per-value literal escape documented. <https://docs.aws.amazon.com/cli/v1/userguide/cli-usage-parameters-file.html> (v1 docs, page says v1 is in maintenance mode). Shorthand `key@=file://x` marks a value as a path, same page.
3. OpenSSL: `pass:`, `env:`, `file:`, `fd:`, `stdin`; `pass:` is "visible to utilities (like 'ps')"; `file:` reads the first line; "fd: is not supported on Windows". <https://docs.openssl.org/3.0/man1/openssl-passphrase-options/>. No bare form.
4. curl: "If you start the data with the letter @, the rest should be a filename"; `--data-raw` removes it; `--form-string` "Leading @ and < characters ... have no special meaning"; `-d @file` strips CR/LF/NUL. <https://curl.se/docs/manpage.html>. HTTPie: `=@file`, backslash escapes. <https://httpie.io/docs/cli/file-based-separators> (the page gives no literal-`@` example).
5. Grafana: `$__env{}`, `$__file{}` ("trims whitespace from the beginning and the end"), `$__vault{}` (Enterprise only). <https://grafana.com/docs/grafana/latest/setup-grafana/configure-grafana/> (first 100k chars read; no escape rule found).
6. 1Password: `op://<vault>/<item>/[section/]<field>` with `?attribute=`; `op run` scans env vars for references, masks printed secrets by default; the page does not say what happens to non-matching values or give escaping rules. <https://www.1password.dev/cli/secret-references/>.
7. Vector: `SECRET[<backend>.<name>]`; backends `exec`, `file`, `directory`, `aws_secrets_manager`; `directory` has `remove_trailing_whitespace` default `false`. <https://vector.dev/docs/reference/configuration/secrets/>. Telegraf: `@{<store>:<name>}`, ids and names `[A-Za-z0-9_]`. <https://github.com/influxdata/telegraf/blob/master/docs/CONFIGURATION.md>.
8. Spring Boot `configtree:` import prefix, one file per key, Docker secrets at `/run/secrets/`. Spring states "Storing config values in environment variables has drawbacks, especially if the value is supposed to be kept secret." <https://docs.spring.io/spring-boot/reference/features/external-config.html>.
9. Home Assistant `!secret id` from `secrets.yaml` (parent-folder search). <https://www.home-assistant.io/docs/configuration/secrets/>.
10. Docker BuildKit `--secret id=aws,src=path` / `id=kube,env=KUBECONFIG`; bare `id` auto-detects an env var. <https://docs.docker.com/build/building/secrets/>.
11. systemd: file-first model, "by default [env vars] are inherited down the process tree, have size limitations, and issues with binary data". <https://systemd.io/CREDENTIALS/>.
12. Not read successfully: Kong reference grammar (page too large, grammar not in first 100k chars; <https://developer.konghq.com/gateway/entities/vault/>), k8s `valueFrom` comparison text (excerpt truncated; <https://kubernetes.io/docs/tasks/inject-data-application/distribute-credentials-secure/>), Pulumi/Terraform, Vault agent, sops/age (README silent).

### 2. S users

1. Docker official images: `file_env` header "(will allow for "$XYZ_DB_PASSWORD_FILE" to fill in the value of "$XYZ_DB_PASSWORD" from a file, especially for Docker's secrets feature)"; if both set: `error: both %s and %s are set (but are exclusive)`, exit 1; the file is read with `$(< file)`. <https://raw.githubusercontent.com/docker-library/postgres/master/docker-entrypoint.sh>. Postgres image: "Currently, this is only supported for POSTGRES_INITDB_ARGS, POSTGRES_PASSWORD, POSTGRES_USER, and POSTGRES_DB." <https://hub.docker.com/_/postgres>.
2. restic: `RESTIC_PASSWORD`, `RESTIC_PASSWORD_FILE` ("overridden by --password-file"), `RESTIC_PASSWORD_COMMAND` ("Command printing the password for the repository to stdout"). <https://restic.readthedocs.io/en/stable/075_scripting.html>. The page states no precedence between `RESTIC_PASSWORD` and the file/command variables.
3. borg: precedence `BORG_NEW_PASSPHRASE` > `BORG_PASSPHRASE` > `BORG_PASSCOMMAND` > `BORG_PASSPHRASE_FD`; "the command is executed without a shell"; `_FD` preferred because "environment can be examined by other processes". <https://borgbackup.readthedocs.io/en/stable/usage/general.html>.
4. rclone: `--password-command` / `RCLONE_PASSWORD_COMMAND` / `RCLONE_CONFIG_PASS`. <https://rclone.org/docs/> (offset 100000 read; Environment variables section not fully read).
5. PostgreSQL client: `PGPASSWORD` "not recommended for security reasons ... instead consider using a password file"; `PGPASSFILE` exists; no `PGPASSCOMMAND` on the page. <https://www.postgresql.org/docs/current/libpq-envars.html>. 2013 pgsql-hackers askpass/command-hook proposal outcome not confirmed (snippet): <https://www.postgresql.org/message-id/CAAZKuFaJUfdDFp1_vGHbDfYRu0Sj6mSOVvKRp87aCQ53ov6iwA@mail.gmail.com>.
6. Gitea `GITEA__SECTION__KEY__FILE` via `environment-to-ini` (snippet, mirrors): <https://github.com/go-gitea/gitea/issues/10311>. Official docs page for env variables does not cover it (read).
7. Grafana `GF_<SECTION>_<KEY>__FILE`: community threads show single-underscore `_FILE` ignored, double-underscore required; an old docs commit showed the single-underscore form (snippet, OLD): <https://community.grafana.com/t/docker-swarm-secret-file-not-working/88921>, <https://forger.sitiv.fr/apitech/grafana/commit/e562ae753b75210a56d98e3689179bebb318d0f7>.
8. MinIO `MINIO_ROOT_PASSWORD_FILE` is cited only through a docs issue (snippet): <https://github.com/minio/docs/issues/1499>. Ansible: vault password via file; executable file is run (summary of a script header comment, not a normative sentence): <https://docs.ansible.com/ansible/latest/vault_guide/vault_managing_passwords.html>. Keycloak, Traefik, Nextcloud, MySQL, Vault: not examined.
9. Do S tools also accept V? Grafana yes (ini `$__file{}` plus env `__FILE`). Spring and Docker Compose-style stacks combine configtree with env. Others read: none found.

### 3. Programs offering both, or that switched

- Grafana offers both; the ini expansion and the env suffix are separate mechanisms (<https://grafana.com/docs/grafana/latest/setup-grafana/configure-grafana/> and community threads above). No sentence found explaining why both.
- Docker secrets guidance across community pages frames `_FILE` as an adapter: "Docker Swarm and Kubernetes both mount secrets as files, but many applications only know how to read configuration from environment variables" (snippet, third-party tool page, not a docker-library statement): <https://hub.docker.com/r/skriptfabrik/fileenv>. No docker-library issue/PR was found arguing `_FILE` against a `file://` scheme.
- No program found that switched from S to V or V to S, with a stated reason. Not found: a migration sentence for any of them.
- AWS: `credential_process` (separate key) coexists with `file://` (V) for parameters, different layers.

### 4. Problems with V

1. Literal collision. AWS offers only a global opt-out (`cli_follow_urlparam`) and auto-disables for URL-typed parameters (same AWS page). curl and HTTPie give a distinct option/escape (`--data-raw`, `--form-string`, `\`). No per-value escape in AWS docs found.
2. Attacker-controlled value reads a file: curl `@`-prefixed body/header. "The @ file-read behaviour works with headers too"; Burp Suite Pro "Copy as curl" and Google Chrome DevTools emitted `--data-binary` with user data; fixed in Burp 2020.5.1 by switching to `--data-raw` when a body starts with `@`. PortSwigger, 2023-03-28: <https://portswigger.net/research/the-curl-quirk-that-exposed-burp-suite-amp-google-chrome>. curl documents the behaviour, so it is not a curl CVE (same page).
3. Template/tag interpretation: Ansible CVE-2017-7481, lookup results not marked unsafe, Jinja2 evaluated them (code execution); fixed in 2.3.1.0 etc. (snippet, OLD 2017): <https://github.com/advisories/GHSA-w578-j992-554x>, <https://nvd.nist.gov/vuln/detail/CVE-2017-7481>. PyYAML: "It is not safe to call yaml.load with any data received from an untrusted source!" and `!!python/object` tags: <https://pyyaml.org/wiki/PyYAMLDocumentation> (OLD docs; generic YAML-tag example, not a secret-reference case).
4. Escaping bugs: Grafana users report `$` in passwords eaten or mis-escaped in config (`$$`/`\$` inconsistent); no resolution (snippets): <https://community.grafana.com/t/escaping-a-dollar-sign-so-it-doesnt-get-treated-as-a-variable/67941>, <https://github.com/oracle/oracle-db-appdev-monitoring/issues/292>.
5. Shell interaction: 1Password notes `$MY_VAR` expands before `op run` sees the reference (same 1Password page).
6. A new scheme later changes existing values: not found as a reported incident. Only a design-level inference; no source.
7. Secret scanners/log redaction when the variable holds a path or reference: not found. 1Password masks printed resolved secrets (1Password page). No source on scanners treating `file://...` values.
8. `file://` ambiguity: curl list thread notes `file://vmlinuz` means `/vmlinuz` while `file://etc/passwd` means `/passwd` (snippet): <https://curl.se/mail/lib-2016-11/0080.html>. Bamboo issue on `file://path` silently translated to `file:/path` (snippet): <https://jira.atlassian.com/browse/BAM-7206>.

### 5. Problems with S

1. Both set: Docker aborts (exclusive); borg/restic/rclone define precedence; other third-party tools disagree (some `_FILE` wins, some only if file readable) (snippet, search summary): <https://github.internet2.edu/docker/comanage-registry-docker/blob/858c1a71229d8c2ba2138f1861732aeb694ebfc2/docs/comanage-registry-common-environment-variables.md>.
2. Proliferation/coverage: Postgres image covers 4 variables only (page above). restic spreads across `_FILE`/`_COMMAND` per secret.
3. Discoverability/naming drift: Grafana `_FILE` vs `__FILE` thread above.
4. Trailing newline: forum reports of login failures from a trailing newline in secret files (snippet, user threads): <https://discourse.linuxserver.io/t/mariadb-creation-with-docker-secrets-not-working/2138>, <https://caddy.community/t/file-placeholder-not-working-as-expected-with-dns-acme-challenge/24596>. Docker `$(< file)` strips trailing newlines by shell semantics (general knowledge, not from a page). Vector `directory` backend leaves whitespace unless `remove_trailing_whitespace`; Grafana `$__file` trims both ends (pages above). GitLab issue on `initial_root_password` containing a newline (snippet): <https://gitlab.com/gitlab-org/gitlab/-/issues/340842>.
5. Environment exposure: PG `PGPASSWORD` and borg `BORG_PASSPHRASE` notes (pages above) concern env-held values, which applies equally to S and V when the variable holds the literal.

### 6. Command sources

- Value-triggered execution: git `credential.helper` leading `!` is a shell snippet; "Any helper string, including a `!` snippet, executes shell commands. Only configure helpers you trust, and be cautious with config that comes from untrusted repositories or includes" (summary) <https://git-scm.com/docs/gitcredentials>. Vector `exec` backend is chosen by backend `type`, not by the reference string (<https://vector.dev/docs/reference/configuration/secrets/>).
- Separate key/variable: AWS `credential_process` (warning: "could be a security risk if the command to generate the credentials becomes accessible by non-approved processes or users") <https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sourcing-external.html>; msmtp `passwordeval` vs `password` ("Consider using the 'passwordeval' command or a key ring instead") <https://marlam.de/msmtp/msmtp.html> (docs give no precedence); mbsync `PassCmd` ("allows you to use password files and agents") <https://isync.sourceforge.io/mbsync.html>; aerc `source-cred-cmd` <https://man.archlinux.org/man/aerc-accounts.5>; restic/borg/rclone `*_COMMAND` variables (above).
- Security reasoning for separating command from value:
  - Kubernetes v1.35 blog, 2026-01-09: kubectl "can run arbitrary executables, including shell scripts, with the full privileges of the invoking user"; adds a kuberc credential-plugin policy (AllowAll/DenyAll/Allowlist per the search snippet): <https://www.kubernetes.io/blog/2026/01/09/kubernetes-v1-35-kuberc-credential-plugin-allowlist/>.
  - Flux CVE-2022-24817 / GHSA-vvmq-fwmg-2gjc: kubeconfig exec commands in user-supplied kubeconfig gave code execution in controllers; fix: disabled by default, opt in with `--insecure-kubeconfig-exec` (snippet, secondary databases; severity 6.5 vs 9.9 depending on source): <https://osv.dev/vulnerability/GHSA-vvmq-fwmg-2gjc>.
  - CVE-2026-62284 (OSAC operator): kubeconfig `exec` stanza in a Secret gave RCE (snippet): <https://corgea.com/advisories/vulnerabilities/CVE-2026-62284>.
  - borg: `BORG_PASSCOMMAND` "executed without a shell" (borg page above).
- No sentence found, in any program read, that states "a value may set a secret but must never run a program" as a design rule; the evidence is the pattern (separate key) plus the kubeconfig/Flux incidents and the `credential_process` warning.
- Windows `credential_process` quoting: `"C:\Path\To\credentials.cmd" param "param with spaces"`; no env vars, no `~` (AWS page above).

### 7. Windows

1. Drive letters: AWS passes `file://C:\temp\filter.json` verbatim (not RFC 8089 `file:///C:/...`); cosign splits only on `://`, so `C:\x` is a bare path (summary of source). RFC 8089 Appendix E: "URIs of the form `file:///c:/path/to/file` are already supported by the 'path-absolute' rule", `file://host/` is non-local, "localhost" is local, UNC strings are encoded as `file://host/share/path` or with an empty authority and a full UNC in the path, and unencoded backslashes in file URIs are forbidden (Appendix E.4, non-normative): <https://www.rfc-editor.org/rfc/rfc8089.html>.
2. UNC: no program page read documents `\\server\share` handling for its V form. aws-cli discussion #6946 is a Git Bash full-path failure for a `file://`-style parameter, not UNC (snippet): <https://github.com/aws/aws-cli/discussions/6946>.
3. OpenSSL: `fd:` unsupported on Windows (OpenSSL page). Spring page: no Windows notes.
4. Single-colon grammars (`file:C:\x`, `env:NAME`) would have the colon of the drive letter after the first colon; no program read documents this case. Inference only.

## Sources (URL — what it covers)

- <https://raw.githubusercontent.com/sigstore/cosign/main/pkg/blob/load.go> — cosign reference loader (summary; verify against tag)
- <https://raw.githubusercontent.com/sigstore/cosign/main/pkg/blob/load_test.go> — tests: bare path, invalid scheme
- <https://docs.sigstore.dev/cosign/key_management/overview/> — KMS/k8s key URIs
- <https://docs.aws.amazon.com/cli/v1/userguide/cli-usage-parameters-file.html> — AWS `file://`, `fileb://`, Windows, opt-out (v1)
- <https://docs.openssl.org/3.0/man1/openssl-passphrase-options/> — pass phrase sources
- <https://curl.se/docs/manpage.html> — `@file`, `--data-raw`, `--form-string`
- <https://httpie.io/docs/cli/file-based-separators> — `=@`
- <https://portswigger.net/research/the-curl-quirk-that-exposed-burp-suite-amp-google-chrome> — `@` injection (2023)
- <https://grafana.com/docs/grafana/latest/setup-grafana/configure-grafana/> — `$__file{}` etc.
- <https://community.grafana.com/t/docker-swarm-secret-file-not-working/88921> — `__FILE` naming (snippet)
- <https://community.grafana.com/t/escaping-a-dollar-sign-so-it-doesnt-get-treated-as-a-variable/67941> — `$` escaping (snippet)
- <https://github.com/oracle/oracle-db-appdev-monitoring/issues/292> — `$` in password (snippet)
- <https://www.1password.dev/cli/secret-references/> — `op://`
- <https://vector.dev/docs/reference/configuration/secrets/> — `SECRET[...]`, exec backend
- <https://github.com/influxdata/telegraf/blob/master/docs/CONFIGURATION.md> — Telegraf secret stores
- <https://docs.spring.io/spring-boot/reference/features/external-config.html> — `configtree:`
- <https://www.home-assistant.io/docs/configuration/secrets/> — `!secret`
- <https://docs.docker.com/build/building/secrets/> — BuildKit secrets
- <https://systemd.io/CREDENTIALS/> — credentials vs env
- <https://raw.githubusercontent.com/docker-library/postgres/master/docker-entrypoint.sh> — `file_env`
- <https://hub.docker.com/_/postgres> — `_FILE` coverage
- <https://restic.readthedocs.io/en/stable/075_scripting.html> — restic variables
- <https://borgbackup.readthedocs.io/en/stable/usage/general.html> — borg precedence
- <https://rclone.org/docs/> — `--password-command`
- <https://www.postgresql.org/docs/current/libpq-envars.html> — `PGPASSWORD`, `PGPASSFILE`
- <https://docs.ansible.com/ansible/latest/vault_guide/vault_managing_passwords.html> — vault password file/script
- <https://git-scm.com/docs/gitcredentials> — `credential.helper` `!`
- <https://docs.aws.amazon.com/cli/latest/userguide/cli-configure-sourcing-external.html> — `credential_process`
- <https://marlam.de/msmtp/msmtp.html>, <https://isync.sourceforge.io/mbsync.html>, <https://man.archlinux.org/man/aerc-accounts.5> — `passwordeval`, `PassCmd`, `source-cred-cmd`
- <https://www.kubernetes.io/blog/2026/01/09/kubernetes-v1-35-kuberc-credential-plugin-allowlist/> — exec plugin risk
- <https://osv.dev/vulnerability/GHSA-vvmq-fwmg-2gjc>, <https://corgea.com/advisories/vulnerabilities/CVE-2026-62284> — kubeconfig exec incidents (snippets)
- <https://github.com/advisories/GHSA-w578-j992-554x> — Ansible template injection (2017, OLD)
- <https://pyyaml.org/wiki/PyYAMLDocumentation> — YAML tag risk (OLD docs)
- <https://www.rfc-editor.org/rfc/rfc8089.html> — file URI, drive letters, UNC
- <https://curl.se/mail/lib-2016-11/0080.html>, <https://jira.atlassian.com/browse/BAM-7206> — `file://` ambiguity (snippets, OLD)
- <https://discourse.linuxserver.io/t/mariadb-creation-with-docker-secrets-not-working/2138>, <https://caddy.community/t/file-placeholder-not-working-as-expected-with-dns-acme-challenge/24596>, <https://gitlab.com/gitlab-org/gitlab/-/issues/340842> — trailing newline (snippets)

## negative:

Searched and not found:
- Any documented per-value escape for a literal `file://` in AWS CLI (only the global `cli_follow_urlparam` switch); the AWS Developer Blog "Best practices for local file parameters" body did not load (nav only).
- A docker-library statement of why `_FILE` was chosen over a scheme.
- Any program that migrated S to V or V to S, with a stated reason.
- A reported incident of a newly added scheme changing the meaning of stored values.
- Secret-scanner or log-redaction behaviour for a secret variable that holds a path or reference.
- A CVE/advisory for AWS CLI `file://` handling.
- Kong `{vault://}` grammar and escaping; k8s `valueFrom` trade-off text; sops/age key variables; Pulumi/Terraform; Vault agent templates; Keycloak, Traefik, Nextcloud, MySQL `_FILE`; UNC handling in any V grammar; the `restic` and `rclone` env tables in full.
- A normative sentence "a value may set a secret but not run a program".

## leads:

- Read the cosign `LoadFileOrURL` history and the issue that argued `file://` support (pkg/blob in sigstore/cosign) to confirm `file://` is rejected on a tagged release.
- Read the rest of the Kong secrets reference and the Grafana expansion code (`pkg/setting`, `expandConfig`) for escape and recursion behaviour.
- Search docker-library/official-images issues for "file_env" origin commit message in docker-library/postgres (2015-2016) for the design reason.
- Check aws-cli GitHub issues for `file://` plus Windows UNC, and `aws-cli` source `awscli/paramfile.py` for the `://` split and drive-letter handling.
- Check kubectl KEP-541 (<https://www.kubernetes.dev/resources/keps/541/>) for the stated reason exec lives in a dedicated field.
- Check sops docs (getsops.io) for `SOPS_AGE_KEY_CMD` precedence, and rclone Environment variables section for `RCLONE_PASSWORD_COMMAND` precedence.
