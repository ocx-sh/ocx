---
title: "Integrations"
description: "Why OCX stores vendor-specific configuration blocks without interpreting them, and how composition carries them through the dependency graph."
---
<!-- doc_type: explanation -->

This page explains why the `integrations` field stores vendor-specific configuration without ever interpreting it. The field shape, key grammar, size caps and interpolation rules are in the [integrations reference][reference-integrations].

Some tools have configuration OCX has no model for at all. Examples are a list of editor extensions, a JetBrains plugin set, a [devcontainer][devcontainer-customizations] fragment and a language-server setting block. Before the `integrations` field, a publisher could only fork the `metadata.json` format or ship a side-channel file that consumers had to know to look for.

`integrations` gives every publisher one reserved place for vendor-specific configuration, keyed by a namespace. OCX never needs to understand what is inside.

OCX stores a payload and enforces two constraints: a well-formed key and a size cap. It resolves its own `${...}` tokens inside string values and carries the whole map through the dependency graph unread. It never looks inside a payload to interpret it, validate it, merge it or flag a conflict between two packages that declare the same namespace. That non-interpretation is the entire point of the field.

What a `com.microsoft.vscode` block should contain is the consuming tool's business, not OCX's. So is the choice of what to do when two different packages each supply one.

## No Merge, Ever {#no-merge}

Suppose two packages in a dependency graph both declare `com.microsoft.vscode`. Both blocks exist, each attributed to its own package, and the consuming application decides what to do with the two. OCX merges nothing: not their extension lists, not their settings objects, not anything.

This mirrors [Cargo's `[package.metadata.<tool>]` table][cargo-metadata-table]. Cargo reserves the namespace and ignores the contents entirely, with no unused-key warning and no schema. It leaves interpretation to whichever tool the key names. `integrations` makes the same trade for OCX packages.

:::note[Compare with devcontainer.json]
The closest analogue is [devcontainer.json's `customizations` property][devcontainer-customizations], and it takes the opposite approach. A dev container may compose several [Features][devcontainer-features]. It concatenates each feature's `customizations.vscode.extensions` array into one list and merges its `customizations.vscode.settings` object into one settings object. That leaves the merge logic to a tool implementor. OCX takes the reverse-DNS-namespace idea and not the merge.
:::

## Namespace Keys {#namespaces}

OCX validates a namespace key far more loosely than it looks. The convention is reverse-DNS (`com.microsoft.vscode`, `com.jetbrains`, `sh.ocx.completions`), but OCX does not enforce that shape. It refuses an empty key and one longer than 128 bytes. It also refuses a key that could not work as a JSON object key or as plain-text terminal output. The exact rules are in [Namespace Keys][reference-integrations-namespace].

The invisible-character rule exists because OCX prints a namespace key verbatim into plain-text output. Without the rule, a crafted key could display as a different namespace than the one a consumer matches against. The bidirectional-override corner of the `Cf` category is the same rendering trick [Trojan Source][trojan-source] documents for source code.

OCX refuses two whole Unicode properties instead of a codepoint list. They are general category `Cf` and `Default_Ignorable_Code_Point`. A hand-maintained list goes stale against every Unicode release. Once a package publishes, the grammar can only be loosened.

A zero-width space, a byte-order mark or a Hangul filler hidden in a key looks identical on screen. A human comparing two namespaces cannot see it.

:::caution[`sh.ocx.` is reserved by convention, not by code]
OCX declares its own first-party namespaces under the `sh.ocx.` prefix, for example `sh.ocx.completions`. Nothing in the key grammar refuses a third-party key that starts with `sh.ocx.`. That matches the documented-not-validated stance of the reverse-DNS convention itself. Third-party packages must not publish under `sh.ocx.*`, but nothing in the grammar stops them.
:::

## Size Caps {#caps}

OCX measures the caps as compact (whitespace-free) JSON. The per-namespace cap covers that one payload. The per-package cap covers the whole map, including every key and its surrounding punctuation.

A namespace over its own cap is rejected first. If every namespace fits but the map as a whole does not, the per-package error fires. The limits are in [Size Caps][reference-integrations-caps].

The caps are raise-only. `metadata.json` is a read-path format, so an already-published package must keep resolving on every future `ocx`. Lowering either cap would un-resolve a package that published successfully under today's limit. Raising costs nothing, because a package that could not publish under a lower cap was never published.

## The `schemaVersion` Convention {#schema-version}

OCX places no schema-versioning requirement on a payload's contents. If a namespace's own format needs to evolve, the recommended convention is a top-level `"schemaVersion"` key inside that namespace's payload. OCX never enforces it:

```json
{
  "integrations": {
    "com.example": { "schemaVersion": 2, "setting": "value" }
  }
}
```

OCX never reads this key. It exists purely as a hint the consuming tool can check before parsing the rest of the payload.

## Who Can Declare Them {#declarers}

Any package a composition admits can declare them, including a [patch companion][patches-guide]. A companion is a package loaded into the environment. It contributes `integrations` on exactly the terms every other package does, with no separate rule and no separate opt-out.

That makes site policy expressible where it belongs. A proxy setting, an internal CA bundle or an extension allowlist that applies to every project on a machine is published once as a companion. Package authors do not restate it. Each row names the declaring package, so a companion's contribution is always distinguishable from the package you asked for. Details and the surface rules live in [Env Composition][env-composition-integrations].

The row's `package` field names the companion's own identifier, never the base it was admitted for. A companion matched against several bases contributes once, as its `env` entries do.

Declining a companion is the existing [`no-patches`][patches-no-patches] opt-out. It drops an optional companion whole, `env` and integrations together. A required companion is required on the same terms its `env` already is.

## What Composition Does {#composition}

Declaring a block is half of the feature. The other half is what [`ocx env`][cmd-env] and [`ocx package env`][cmd-package-env] do with the blocks of every package they admit.

Composition never merges. OCX cannot know which of two colliding `settings` objects should win. Instead of guessing, it keeps both, each attributed to the package that declared it. A consumer that needs to reconcile two blocks does so itself. See [No Merge, Ever](#no-merge) for the comparison with devcontainer.json.

### Rows Stay an Array {#composition-rows}

The composed JSON envelope carries `integrations` as one of several top-level arrays, alongside `items`, `binaries`, `entrypoints` and `advisories`. It is never nested inside `items`. The array is present with attribution even when composing a single package, and is never collapsed to a bare object. The one filter idiom that works against a multi-package composition is `.integrations[] | select(.namespace=="...")`. It is then the only idiom anyone needs to learn.

Two packages declaring `com.microsoft.vscode` produce two rows with the same `namespace` and different `package`. An array longer than its distinct-namespace count is the visible proof that nothing merged. OCX interpolates each payload against its own package's `${installPath}`, never the composing root's. The fields are listed in the [row shape reference][env-composition-row-shape].

### Interface Surface Only {#composition-surface}

Integrations reach only the interface surface, at every depth. `--self`, and the self view a generated launcher forces, always compose `"integrations": []`.

That differs from `binaries` and `entrypoints`, whose surface membership follows the two-axis visibility each declared entry carries. An integrations block has no `visibility` field, because it belongs to the interface surface structurally and not by a declared axis.

A dependency reached only through a `private` edge contributes nothing on either surface. The same edge rule already governs when its `env` vars cross.

### Not in Shell or CI Output {#composition-shell-ci}

Neither `--shell` nor `--ci` output carries an `integrations` representation. A shell export line and a CI sink's key/value pair have no shape for an arbitrary JSON payload. Inventing one, such as a serialized blob packed into an env var, would be a second wire format. The JSON envelope already has one, so only `--format json` carries the array.

<!-- external -->
[cargo-metadata-table]: https://doc.rust-lang.org/cargo/reference/manifest.html#the-metadata-table
[devcontainer-customizations]: https://containers.dev/implementors/json_reference/
[devcontainer-features]: https://containers.dev/implementors/features
[trojan-source]: https://trojansource.codes/

<!-- reference -->
[reference-integrations]: ../reference/metadata/integrations.md#integrations
[reference-integrations-namespace]: ../reference/metadata/integrations.md#integrations-namespace
[reference-integrations-caps]: ../reference/metadata/integrations.md#integrations-caps
[env-composition-integrations]: ../reference/env-composition.md#integrations-companions
[env-composition-row-shape]: ../reference/env-composition.md#integrations-row-shape

<!-- guides -->
[patches-guide]: ../guides/patch-packages.mdx
[patches-no-patches]: ../guides/patch-packages.mdx#patches-no-patches

<!-- commands -->
[cmd-env]: ../reference/command-line/commands.md#env-root
[cmd-package-env]: ../reference/command-line/package-consuming.md#package-env
