# YAML / JSON Templates

Reference templates for the files generated in Phase 4 of the mirror creation workflow.

## metadata.json

Write to `mirrors/{name}/metadata.json` (and platform variants like `metadata-darwin.json` if needed):

```json
{
  "type": "bundle",
  "version": 1,
  "env": [
    {
      "key": "PATH",
      "type": "path",
      "required": true,
      "value": "${installPath}/bin"
    }
  ]
}
```

## mirror.yml

Write to `mirrors/{name}/mirror.yml`:

```yaml
name: {name}
target:
  registry: {registry}
  repository: {repository}

source:
  type: github_release
  owner: {owner}
  repo: {repo}
  tag_pattern: "{pattern}"

assets:
  {platform}:
    - "{regex}"
  # ... per detected platform

# Only include if archives have a top-level wrapper directory
# strip_components: 1

metadata:
  default: metadata.json
  # platforms: ... (only if platform-specific metadata needed)

skip_prereleases: true
cascade: true
build_timestamp: none

versions:
  min: "{min_version}"
  new_per_run: 10

verify:
  github_asset_digest: true

concurrency:
  max_downloads: 8
  max_bundles: 4
  max_pushes: 2
  rate_limit_ms: 100
  max_retries: 3
  compression_threads: 0
```

## README with frontmatter

Generate `mirrors/{name}/README.md`:

```markdown
---
title: {display_name}
description: {one_line_description}
keywords: {comma_separated_keywords}
---

# {display_name}

{2-3 sentence description of what the tool is and does. Research the project's
GitHub description and website to write an accurate summary.}

## What's included

{List the main executables or components included in the package. Derive this
from the archive inspection in Phase 3.}

## Links

- [{display_name} Documentation]({docs_url})
- [{display_name} on GitHub](https://github.com/{owner}/{repo})
```

**Frontmatter fields:**

- `title`: Human-readable display name (e.g. "CMake", "Go Task", "Buf")
- `description`: One-line summary suitable for catalog display (max ~100 chars)
- `keywords`: Comma-separated search terms — include the tool name, language ecosystem, and category (e.g. `cmake,build,cpp,c,build-system,cross-platform`)

**Body content:**

- Research the project by reading its GitHub description (`gh api "repos/{owner}/{repo}" --jq '.description'`) and website
- List executables found during archive inspection
- Include links to docs and GitHub
- Do NOT include a "Usage with OCX" section — the website's DetailView already provides install/exec commands

## Taskfile registration

Add the mirror to `taskfiles/mirror.taskfile.yml`. This file includes the shared template (`mirrors/mirror.taskfile.yml`) once per package. Add a new `includes` entry and wire it into `sync-all` / `describe-all`:

```yaml
# In the includes: block, add:
  {name}:
    taskfile: ../mirrors/mirror.taskfile.yml
    vars:
      PACKAGE: {name}
      REPO: {registry}/{repository}

# In tasks.sync-all.cmds, add:
      - task: {name}:sync

# In tasks.describe-all.cmds, add:
      - task: {name}:describe
```

This gives the user `task mirror:{name}:sync` and `task mirror:{name}:describe` automatically via the shared template.
