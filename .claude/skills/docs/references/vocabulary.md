# Docs Vocabulary

One term per concept. The full approved list is the conflict table C1-C46 in `.claude/artifacts/research_docs_glossary.md` (data: `research_docs_glossary_terms.json`); decisions in `.claude/artifacts/adr_docs_glossary.md`. The table below holds the decisions writers trip on. A word used in a second sense is reworded, not explained.

| Topic | Write |
|---|---|
| Command families | "the `ocx package` commands" (code font; the identifier-taking family) or, as a modifier, "`ocx package` command behavior". The other family is "the project commands" (`ocx add`, `ocx lock`, `ocx pull`, `ocx exec`, ...). Never bare "package commands", never "OCI tier". Same wording in Rust help strings. |
| tier | Config tiers only: "system tier", "user tier", "OCX home", "managed" (hyphen only as a compound modifier: "system-tier lock"). Anything else is reworded: the project, the `[patches]` section, the directories GC names. |
| rung / ladder | Only on a page that names a ladder (resolution, activation) and only for that ladder's steps; never for config tiers. |
| render marker | "render marker" in prose; the file `render_stamp.json` and anchors keep their names. "consent stamp" stays. |
| lock candidate | "the lock `ocx update` would write"; not "trial lock". |
| snapshot of the environment | "a copy of the environment taken at spawn time". |
| `--ci` sink | "CI output file(s)"; where GitHub is named, "the runner's `GITHUB_PATH` and `GITHUB_ENV` files". |
| roles | operator = configures hosts, publishes patches or managed config; publisher = builds and pushes a package; owner = an index `login:id`; "maintainer" only for upstream project maintainers. |
| Windows executables | launcher = a generated native executable that starts a program from its record `NAME.launch`, on every OS; its `kind` key names the role: `package` (an entry point), `deferred` (the placeholder of a deferred package, a "shim"), `toolchain` (a "trampoline" in the toolchain tree that re-enters `ocx exec`) or `direct` (a vendored program). One blob backs every launcher: "the shared `ocx-shim` executable blob". |
| env files | "env file(s)" for `env.sh`, `env.elv`, ...; never "shim". |
| fixed-project session | "a session fixed to the project it was opened in", defined once where it first appears; no flag named. |
| integration key | "integration key" in prose; the JSON field is `namespace`, named in code font when meant. |
| lock file | "lock file" = `ocx.lock`; not "lockfile", not a mutex ("never writes a mutex file"); `pylock.toml` is named by its file name. |
| object store | "object store" (the CLI help's word) for the content-addressed store; not blob store, package store or similar. |
| rolling tag, platform manifest, root document | "rolling tag" (not floating, moving, alias, cascade alias); "manifest" means an OCI manifest only (never `ocx.toml`), a per-platform one is a "platform manifest"; "root document" (not "index root"). |
| synced copy | the local copy of managed config; "managed copy" only for the self-install. |
| product name | "OCX" in prose, headings, titles and link text; `ocx` in code font only for the command or binary. The lowercase wordmark stays in the site title, logo and README heading. `website/tests/product-name.test.mjs` enforces it. |
| spelling | "materialize" (z). "entry point" as a noun, "entry-point" only as a modifier. |

Decided: the reader column of an env variable is labelled "Audience" (C16); `ocx remove` takes a `<BINDING>` (D8).

## The glossary

`website/src/data/glossary.json` holds one row per term; the build links the terms in prose (`ocxTheme({ glossary })` in `website/astro.config.mjs`, every usage) and `reference/glossary.md` is generated from it.

- **Write the plain term in prose.** Never hand-link it, never wrap it in `<Tooltip>`; the build does both. To change where a word links or what it says, edit its row, not the page.
- **Add or change a term:** edit the row (keep the file sorted by category, then term, 2-space indent), run `task website:glossary:generate`, commit the page with the data. `task website:test` fails when they differ.
- **Row fields:** `definition` at most 25 words, plain vocabulary; `learnMore` a docs route ending in `/` with an optional `#anchor`, or an `http(s)` URL; `variants` every form that links (inflections spelled out; an uppercase letter makes a form match exactly, all-lowercase matches any case; the longest form wins); `avoid` retired synonyms that the docs lint rejects in hand-written pages; `except` phrases that contain a variant but are not the term (`package manager`); `excludePages` route prefixes (`integration/github-actions/`) on whose pages the term never links.
- **Never linked:** headings, link text, code, raw HTML, and the content of components other than `Steps`, `Tabs`, `TabItem`, `Aside`, `details`. A term split across inline markup is not linked.
- **A link in the wrong sense:** every usage links, so a word used in another sense is fixed in this order: (1) say the full term the sentence means (`image index`, `patch descriptor`), which links to its own row; (2) an `except` phrase when a qualifier word sits next to the term in continuous text (`Git repository`), written to start at the term so it does not swallow a neighbouring link, with both apostrophe forms; (3) `excludePages` when the bare word means something else on a whole page or the page is generated; (4) a minimal reword of one hand-written sentence. Generated pages (`reference/command-line/**` and the four single pages) take tool 2 or 3 only. `website/tests/glossary.test.mjs` fails an `excludePages` prefix that matches no page.
- **Page budget:** each link is one DOM element. Frequency and the budget → the comment beside `glossary` in `astro.config.mjs`.
