# OCX Pitch & Messaging Guide

> Reference sheet for anyone writing about, presenting, or promoting OCX.
> Keep this open when drafting docs, landing pages, talks, or sales decks.

---

## 1. Lead With Pain, Not Features

Every pitch should start from a problem the audience already feels. Don't open with what OCX *is* — open with what's broken today.

| Audience | Pain point to lead with |
|---|---|
| Platform / DevEx teams | "Distributing internal CLI tools across engineering is a patchwork of curl scripts, GitHub Releases, and tribal knowledge." |
| CI/CD engineers | "Your build breaks when a download URL changes, a CDN goes down, or someone pushes a new `latest` tag." |
| Security / Compliance | "You can't audit what you can't pin. GitHub Release URLs are mutable. Homebrew taps auto-update without consent." |
| Leadership / Budget | "You're paying for Artifactory licenses to distribute binaries your OCI registry could host for free." |

**Anti-pattern:** Don't open with "OCX is an OCI-based package manager." That's the *what*, not the *why*. The audience doesn't care about OCI — they care about their broken deploy pipeline.

---

## 2. The Elevator Pitch (Adapt Per Audience)

### Generic (10 seconds)
> OCX turns the OCI registry you already run into a cross-platform binary package manager — no new infrastructure, no new costs.

### For platform teams (30 seconds)
> Every org distributes internal tools — CLIs, linters, code generators. Today that means GitHub Releases with curl scripts, internal Homebrew taps, or "just put it on the wiki." OCX lets you `push` a binary to your existing OCI registry and `install` it on any platform with one command. Same auth, same RBAC, same infrastructure you already have.

### For CI/CD (30 seconds)
> CI should be deterministic. But your tool setup step downloads from URLs that change, CDNs that go down, and tags that silently mutate. OCX pins tools by content digest and works offline from a local index snapshot. Same snapshot, same binaries, every time — even if the internet is on fire.

---

## 3. Key Messages — Say This

| Message | Why it works |
|---|---|
| **"You already have the infrastructure."** | Removes the #1 adoption objection (cost/ops burden). Every container shop has an OCI registry. |
| **"One command, any platform."** | Contrasts with the reality of per-OS download scripts and architecture matrices. |
| **"Reproducible by default, not by discipline."** | Positions against tools where reproducibility requires careful manual pinning. |
| **"Ship internal tools like container images."** | Maps to a workflow the audience already knows. Lowers cognitive barrier. |
| **"No new servers. No new licenses. No new credentials."** | Triple reinforcement of zero-infrastructure-cost. |

---

## 4. Comparisons — Handle With Care

### DO: Use honest, specific comparisons

- "Unlike Homebrew, OCX supports private binaries as a first-class use case — Homebrew explicitly disclaims interest in private software."
- "Like Nix's content-addressed store, but without learning a functional programming language."
- "ORAS moves blobs. OCX manages packages — with versioning, environment variables, and symlink-based version switching."

### DON'T: Trash competitors or overclaim

- Never say another tool is "bad" — say OCX is "designed for a different use case."
- Never imply OCX replaces apt/dnf for system packages. It doesn't. It's for *tool* binaries.
- Never call OCI tags "immutable" or "frozen" — they are mutable by spec. OCX uses digest pinning for immutability, and build-tagged conventions for stability. Be precise.

### Comparison anchors (use these analogies)

| OCX concept | Familiar analogy | Use when explaining... |
|---|---|---|
| Object store | Nix store, Git objects | Why identical binaries aren't duplicated |
| Local index | `apt-get update` / APT package lists | Why installs work offline and are reproducible |
| Candidate/current symlinks | SDKMAN, `update-alternatives`, Homebrew Cellar+opt | How version switching works without moving files |
| Digest pinning | Docker `image@sha256:...`, Go module checksums | How to get absolute reproducibility |
| Cascade tags | Docker official images (`ubuntu:24.04` → `ubuntu:latest`) | How semver-like tag hierarchies work |

---

## 5. Objections — Anticipate and Address

| Objection | Response |
|---|---|
| "Why not just use Docker?" | Docker distributes *environments* (OS + deps + app). OCX distributes *tools* (single binaries you run on the host). Different mental model, different UX. |
| "We already use Homebrew/mise." | Great for public tools on developer desktops. OCX shines for *internal* tools, *CI pipelines*, and *multi-platform* distribution — where those tools fall short. |
| "What about Nix?" | Nix is powerful but demands learning a functional language. OCX offers the same content-addressed storage model on infrastructure you already run, with a CLI you can learn in 5 minutes. |
| "Is this vendor lock-in?" | Packages are standard OCI artifacts. ORAS, crane, skopeo, and Docker CLI can all read them. No proprietary format. Walk away anytime. |
| "Where's the package catalog?" | OCX is BYO-registry. For public packages, we maintain a curated set on `ocx.sh`. For internal tools, *you* are the publisher and the consumer — that's the point. |
| "Is it production-ready?" | Be honest about maturity stage. Point to the test suite, the architecture, and the design decisions. Don't overclaim. |

---

## 6. Messaging Don'ts

- **Don't say "revolutionary" or "game-changing."** Let the zero-cost infrastructure story speak for itself.
- **Don't lead with OCI.** Most people don't know or care what OCI is. Lead with the outcome ("uses your existing Docker registry"), explain OCI only if asked.
- **Don't compare to system package managers.** OCX doesn't replace apt. Comparing invites unfavorable scrutiny on ecosystem size.
- **Don't promise a public package ecosystem (yet).** The strength is internal/private distribution. A public catalog is a future goal, not today's selling point.
- **Don't use "simple" or "easy" without proof.** Show a 3-line install, don't assert simplicity. The reader decides if it's simple.
- **Don't hand-wave security.** Corporate buyers will probe. Be specific: digest verification, TLS-only by default, no code execution on install, registry-native auth/RBAC.

---

## 7. Proof Points to Include

Concrete demonstrations beat abstract claims. When possible, include:

- **Side-by-side command comparison.** Show the curl/tar/chmod dance vs. `ocx install tool:1.2`. Visual contrast is powerful.
- **Offline demo.** Show an install working with no network. This surprises people.
- **Multi-platform resolution.** Show the same command producing different binaries on linux/amd64 vs. darwin/arm64.
- **CI before/after.** Show a GitHub Actions workflow shrinking from 20 lines of download logic to 2 lines of `ocx install`.
- **Private registry flow.** `ocx package push` to a private ECR/GHCR, `ocx install` from another machine with the same credentials. End to end in under a minute.

---

## 8. Technical Precision Checklist

When writing docs or technical content, verify these nuances:

- [ ] OCI tags are **mutable**. Never say a tag is "frozen" — use "conventionally stable" for build-tagged versions.
- [ ] Digest pinning provides **absolute** reproducibility. Tags provide **conventional** reproducibility. Make the distinction.
- [ ] The local index is a **snapshot**, not a cache. It doesn't auto-update. This is a feature, not a limitation.
- [ ] `--cascade` is a **publisher convention**, not a registry-enforced guarantee. The registry doesn't know about semver.
- [ ] Content-addressed means **any** package can be locked by digest, regardless of tag scheme. Mention this before describing tag-based locking.
- [ ] Clean env execution means **only** package-declared variables, not "isolated" in a container sense. Be precise about the boundary.
- [ ] macOS binaries get **ad-hoc code signed** after extraction. This is automatic and necessary — unsigned binaries trigger Gatekeeper. Don't omit this from macOS docs.

---

## 9. Beachhead Strategy

Focus adoption efforts in this order:

1. **Internal tool distribution** — lowest friction, clearest pain, no ecosystem dependency.
2. **CI/CD tool setup** — measurable value (faster, reproducible builds), integrates with existing workflows.
3. **GitHub Actions / Bazel rules** — multiplier effect (one integration serves many users).
4. **Public package catalog** — only after the above are proven. Don't spread thin.

---

## 10. One-Liner Variants

Pick the right one for the context:

| Context | One-liner |
|---|---|
| GitHub README | A cross-platform binary package manager built on OCI registries. |
| Conference talk | What if your Docker registry could also be your package manager? |
| Blog post | The package manager that runs on infrastructure you already have. |
| Internal pitch | Stop maintaining curl scripts. Push binaries to the registry, install them anywhere. |
| Technical audience | Content-addressed binary distribution over the OCI distribution spec. |
