# Research: Zero-dependency SDK codegen per language

## Metadata

- Date: 2026-09-25
- Method: web scan (WebSearch/WebFetch), no local codegen run performed
- Question: can a published SDK consume JSON described by JSON Schema draft 2020-12
  (`oneOf`+`const` discriminated unions, `type: [x, "null"]` nullable fields,
  optional-vs-absent keys, open/extensible enums) with **zero runtime third-party
  dependencies**, using an **existing off-the-shelf generator** — per language:
  Python, TypeScript, Rust, Go, Swift, Java.

---

## Python

**1. Stdlib JSON facility**: `json` (parse/serialize) + `dataclasses` or `typing.TypedDict`
for shape. `json.loads` returns plain `dict`/`list`/scalars — a `TypedDict` is
*structurally* just that same `dict` at runtime (no conversion step), so
`json.loads(s)` cast to a `TypedDict` type is genuinely zero-dep and zero-code.
`dataclasses` are real objects, so nested `dict → dataclass` needs a manual
recursive constructor (a few stdlib-only lines) or a third-party helper
(`dacite`, `cattrs`) for convenience.

**2. Generator**: [datamodel-code-generator](https://github.com/datamodel-code-generator/datamodel-code-generator)
— `--output-model-type` accepts `pydantic_v2.BaseModel`, `pydantic_v2.dataclass`,
`dataclasses.dataclass`, `typing.TypedDict`, `msgspec.Struct`
([docs](https://datamodel-code-generator.koxudaxi.dev/output-model-types/)).
  - `dataclasses.dataclass` and `typing.TypedDict` → **stdlib only**, zero runtime deps.
  - `pydantic_v2.*` and `msgspec.Struct` → **require** their respective third-party
    packages at runtime (msgspec explicitly flagged as "pip install msgspec" in the docs).
  - For `dataclasses.dataclass` output the generator does **not** emit a
    `from_dict`/decode method — plain dataclass definitions only; nested-object
    decoding from a raw dict is left to the caller.
- Discriminated unions: generator translates `oneOf`+`const` typically to a plain
  `Union[...]` type hint for both dataclasses and TypedDict — no runtime tag dispatch
  is generated for the stdlib-only outputs (Pydantic's own validator does that work
  for `pydantic_v2.BaseModel`, but that output isn't zero-dep).
- Nullable vs absent: `typing.TypedDict` output can use PEP 655 `Required[T | None]`
  (must be present, may be null) vs `NotRequired[T]` (may be absent) — a real,
  checkable static distinction stdlib supports since Python 3.11
  ([PEP 655](https://peps.python.org/pep-0655/)). `dataclasses` cannot express
  "absent" at all (only `Optional[T] = None`, which collapses "explicit null" and
  "not set").
- Open enums: neither output type enforces enum membership at runtime by default
  (TypedDict values are unchecked `dict` values; dataclasses use `Enum`/`Literal`
  type hints that Python does not validate at construction unless you write the check).

**3. Zero-dep async subprocess**: `asyncio.create_subprocess_exec()` /
`create_subprocess_shell()` — both stdlib
([docs](https://docs.python.org/3/library/asyncio-subprocess.html)), return a
`Process`; `await proc.communicate()` or `await proc.wait()` completes it. Fully
zero-dep, no runtime library needed for the async model itself (`asyncio` ships
with CPython).

---

## TypeScript

**1. Stdlib JSON facility**: `JSON.parse`/`JSON.stringify` are ECMAScript builtins
(available identically in browser and Node) — zero-dep by construction. There is
no runtime type-checking layer in the language itself: TypeScript's types are
erased at compile time, so "consuming per the schema" at *runtime* is only as
strong as whatever validation code exists alongside the types.

**2. Generators**:
  - [json-schema-to-typescript](https://github.com/bcherny/json-schema-to-typescript)
    emits **type-only** `.d.ts`/`.ts` interfaces — supports `oneOf`, `$ref`,
    composition keywords. Zero runtime deps because it emits *no runtime code at
    all*: it's a compile-time cast, not a decoder — a payload that violates the
    schema at runtime is silently accepted (no check exists to reject it).
  - [quicktype](https://github.com/glideapps/quicktype) TypeScript target: `--just-types`
    mode emits type-only declarations (same zero-runtime-check caveat as above);
    ordinary mode emits self-contained `Convert.toX`/`cast` functions with **no
    imports** — a real runtime validator, still zero third-party deps, just more
    generated code performing the checks by hand.
  - Known quicktype defect: `oneOf` is not enforced as exclusive-or — generated
    types/validators allow constructing values satisfying more than one branch
    simultaneously, i.e. discriminated-union exclusivity is not guaranteed
    ([glideapps/quicktype#1103](https://github.com/glideapps/quicktype/issues/1103),
    [#953](https://github.com/glideapps/quicktype/issues/953)). Regressions have
    also hit `--just-types` itself across releases
    ([glideapps/quicktype#1200](https://github.com/glideapps/quicktype/issues/1200),
    [#1399](https://github.com/glideapps/quicktype/issues/1399)).
- Nullable vs absent: TypeScript's structural type system distinguishes `x: T | null`
  (present, may be null) from `x?: T` (may be absent) at the type level; both
  generators map `type: [x, "null"]` to `T | null` and JSON Schema's non-`required`
  keys to `?:`. Enforcement at runtime again depends on whether the "just types" or
  the validating mode was used.
- Open enums: `--just-types`/type-only output does not reject unknown values at
  runtime; quicktype's `Convert` mode can be made to reject or accept unknowns
  depending on schema shape, but there's no first-class "open enum" concept in
  either generator.

**3. Zero-dep async subprocess**: `node:child_process` + `node:util.promisify`
(both builtin) — `promisify(exec)`/`promisify(execFile)` give an awaitable
subprocess call ([Node docs](https://nodejs.org/api/child_process.html)); no
third-party package required.

---

## Rust

**1. Stdlib JSON facility**: **none.** `std` has no JSON parser/serializer at all
— every Rust JSON path goes through a third-party crate (`serde_json` being the
de facto standard). This makes "zero runtime deps" categorically unreachable for
JSON handling in Rust, regardless of generator choice.

**2. Generator**: [typify](https://github.com/oxidecomputer/typify) (`cargo typify`)
compiles JSON Schema to Rust types with **deep serde integration** — `oneOf` maps
to Rust `enum`s using [serde's enum representations](https://serde.rs/enum-representations.html)
(e.g. internally-tagged for a `const`-tag discriminator), optional properties become
`Option<T>` with `#[serde(default)]`. Generated code carries `#[derive(Serialize,
Deserialize)]` and other `#[serde(...)]` attributes, so the **consuming crate must
depend on `serde` (derive feature) and `serde_json` at runtime** — this is a hard
requirement of the generated output, not optional. (Typify's own crate listing
`serde`/`schemars` as *dev*-dependencies on crates.io describes typify's own test
suite, not the dependency footprint it imposes on code it generates.)
- Nullable fields (`type: [x, "null"]`): the README's coverage of this exact
  JSON-Schema-2020-12 form was not confirmed in the fetched docs; general Rust/serde
  practice maps such unions to `Option<T>`, same representation as an absent+optional
  property — i.e. Rust's own type system, like Go's, cannot natively distinguish
  "explicit null" from "absent" without a wrapper type (serde has no stdlib-blessed
  answer here either; crates like `serde_with`'s `double_option` fill the gap, itself
  a third-party dependency).
- Open enums: not confirmed from the fetched material; Rust `enum`s are closed by
  default, so an "unknown variant" typically needs a catch-all `#[serde(other)]`
  arm, which typify may or may not emit automatically — flagged as unverified, see
  `leads:` below.

**3. Zero-dep async subprocess**: Rust has **no stdlib async executor at all** —
any `async fn` requires a third-party runtime to be polled to completion, so a
strictly zero-dep *async* subprocess call does not exist in Rust; `std::process`
(`Command::spawn`/`wait`) is synchronous-only. The closest to "not tokio" is
[async-process](https://github.com/smol-rs/async-process) (smol-rs ecosystem),
which avoids pulling in tokio specifically but is still a third-party runtime
dependency (uses `async-io`/`blocking` under the hood). `tokio::process` is the
mainstream option, normally pulled in behind a feature flag
(`tokio = { version = "...", optional = true, features = ["process"] }`) so a
library can make it opt-in for consumers who already run tokio.

---

## Go

**1. Stdlib JSON facility**: `encoding/json` — fully stdlib,
[pkg.go.dev/encoding/json](https://pkg.go.dev/encoding/json). Well known limitation:
it does **not** distinguish "field absent" from "field present with `null`" out of
the box; both collapse to the zero value on a plain (non-pointer) field, and even
pointer fields need care — this is a long-standing tracked issue
([golang/go#44023](https://github.com/golang/go/issues/44023)). The idiomatic
zero-dep workaround is `*T` (nil = absent-or-null, still ambiguous) or a small
hand-rolled wrapper struct tracking a `set bool` flag; several third-party
"nullable" packages exist (`oapi-codegen/nullable`, `nicheinc/nullable`) precisely
because the stdlib gap is real
([jvt.me writeup](https://www.jvt.me/posts/2024/01/09/go-json-nullable/)).

**2. Generator**: [go-jsonschema](https://github.com/omissis/go-jsonschema) —
generates Go struct/type definitions plus unmarshaling code; per its README, `oneOf`
is a listed-supported validation keyword, and the tool's model is: generated
unmarshalers enforce shape-level constraints (required, enum, pattern, bounds,
`additionalProperties: false`) directly, while `oneOf`/`not`/`multipleOf`/
`uniqueItems` are left to a separate validator step rather than baked into
unmarshal. The README did not explicitly confirm zero-third-party-import generated
output in the fetched excerpt, but `encoding/json`-based codegen of this kind is
standard practice in the Go ecosystem — flagged for a closer read, see `leads:`.
- Open enums: not documented in the fetched README section; Go `enum`-like types
  generated from `enum` schemas are typically plain `string`/`int` type aliases
  with named constants, which are structurally open (an unrecognized value just
  decodes into the underlying type) unless the generator adds an explicit
  validation switch.

**3. Zero-dep async subprocess**: no distinct "async" story needed — Go's
concurrency model is goroutines over synchronous calls. `os/exec` (`exec.Command`,
`exec.CommandContext`) run in a goroutine with `cmd.Wait()` blocking that goroutine
gives the same effect as "await" in other languages, entirely stdlib
(`context` + `os/exec`).

---

## Swift

**1. Stdlib facility**: `Codable` + `Foundation.JSONDecoder`/`JSONEncoder`.
`Codable` is core Swift standard library; `Foundation` ships with every Swift
toolchain (on Linux via `swift-corelibs-foundation`) and requires no `Package.swift`
dependency declaration — treated here as toolchain-provided, not a third-party
package. `Codable` does **not** natively support `oneOf`/discriminated unions or
JSON Schema's `type: [x, "null"]` form; both require a hand-written
`init(from decoder:)` that reads the discriminator key via a keyed container and
switches on it — a few lines of stdlib-only code, but not automatic.

**2. Generators**:
  - quicktype's Swift (`Codable`) target: produces `Codable` structs with matching
    `CodingKeys`; per quicktype's own materials the generated models are described
    as dependency-free ([quicktype.io/swift](https://quicktype.io/swift),
    [glideapps/quicktype](https://github.com/glideapps/quicktype)) — zero runtime deps,
    same union-exclusivity caveats noted under TypeScript apply generically
    (quicktype's `oneOf` handling is shared across targets).
  - [swift-openapi-generator](https://github.com/apple/swift-openapi-generator)
    (Apple, OpenAPI not raw JSON Schema, but the closest official generator) is
    explicitly **not** zero-dep: generated code imports `OpenAPIRuntime`, a
    separate published Swift package
    ([swift-openapi-runtime](https://github.com/apple/swift-openapi-runtime)) that
    must be added to `Package.swift` — a real runtime dependency, by design, to
    keep the generator itself dependency-light rather than the *output*.
- Nullable vs absent: `Codable`'s `decodeIfPresent` distinguishes "key absent" from
  "key present" at the container level, and a further `decodeNil()` check
  distinguishes explicit JSON `null` — so the stdlib mechanism *can* express all
  three states, but only if the (generated or hand-written) `init(from:)` uses
  `decodeIfPresent` + `decodeNil()` rather than relying on `Optional` synthesis alone.

**3. Zero-dep async subprocess**: the legacy `Foundation.Process` API predates
Swift concurrency (completion-handler based, no native `async`/`await`). The
maintained forward path is [swift-subprocess](https://github.com/swiftlang/swift-subprocess)
(reached 1.0), a Swift-native `async`/`await` package supporting macOS, Linux, and
Windows with CI on Ubuntu 22.04/24.04, RHEL UBI9, Debian 12, Amazon Linux 2023 —
but it ships as a **separate SwiftPM package**, i.e. a real dependency, not part
of core Foundation/stdlib today. A zero-dep async wrapper around the old
`Process` is possible by hand (wrap the completion handler in
`withCheckedContinuation`), but that is hand-rolled, not off-the-shelf.

---

## Java

**1. Stdlib facility**: **none.** The JDK ships **no built-in JSON parser** —
`javax.json`/JSR-353 (Java API for JSON Processing) was a Java **EE** standard,
never folded into core `java.base`, so it is not "stdlib" for a plain JDK/SE
application ([Oracle JSR-353 page](https://www.oracle.com/technical-resources/articles/java/json.html),
[innoq writeup](https://www.innoq.com/en/blog/2022/02/java-json/)). Every JSON
path in Java therefore requires a third-party library (Jackson, Gson, org.json,
Moshi, …) — same categorical gap as Rust, for the same reason (no stdlib format).

**2. Generator**: [jsonschema2pojo](https://github.com/joelittlejohn/jsonschema2pojo)
— `annotationStyle` accepts `jackson`, `jackson2`, `jackson1`, `gson`, `moshi1`,
and **`none`** (no binding annotations at all,
[AnnotationStyle.java](https://github.com/joelittlejohn/jsonschema2pojo/blob/master/jsonschema2pojo-core/src/main/java/org/jsonschema2pojo/AnnotationStyle.java)).
Critically, **`none` only removes the annotations from the generated POJOs** — it
does not remove the need for a JSON library at runtime, because Java's `Class`
reflection alone cannot deserialize arbitrary JSON into a POJO without either
annotations feeding a library (Jackson/Gson) or hand-written parsing code. So even
the "annotation-free" mode is not zero-dep; it just defers the choice of binding
library (or hand-rolled reflection code) to the consumer. Java `record`s (16+) are
a natural fit for immutable generated types, but face the identical gap: no
stdlib deserializer exists to populate a `record` from JSON.
- Discriminated unions / nullable / open enums: jsonschema2pojo's handling of
  these is entirely a function of the chosen `annotationStyle` (e.g. Jackson's
  `@JsonTypeInfo`/`@JsonSubTypes` for polymorphism) — none of that carries over to
  the `none` style, so achieving `oneOf`+`const` dispatch in annotation-free
  output requires hand-written `equals`/factory logic.

**3. Zero-dep async subprocess**: `ProcessBuilder` + `Process.onExit()` — since
Java 9, `onExit()` returns a `CompletableFuture<Process>` that completes on
process termination regardless of exit status, and supports `.thenAccept(...)`
chaining ([Process (JDK 9) docs](https://docs.oracle.com/javase/9/docs/api/java/lang/Process.html),
[onExit guide](https://docs.oracle.com/en/java/javase/22/core/managing-processes-asynchronously-onexit-method.html)).
Fully stdlib, zero third-party deps.

---

## Where zero-dep is infeasible — common practices, with examples

1. **Shading/relocating a dependency inside the published artifact (Java).**
   AWS SDK for Java v2 ships a **pre-shaded** `software.amazon.awssdk:third-party-jackson-core`
   artifact — Jackson's classes relocated under the AWS SDK's own package
   namespace via `maven-shade-plugin`, so the SDK's consumer never sees a plain
   `com.fasterxml.jackson` classpath entry or version-conflict risk
   ([Maven Central listing](https://search.maven.org/artifact/software.amazon.awssdk/third-party-jackson-core),
   [relocation issue discussion](https://github.com/aws/aws-sdk-java-v2/issues/4391)).
   The dependency is still there at the bytecode level; shading only hides it
   from the consumer's own dependency graph and avoids classpath collisions.

2. **Vendoring (Go).** `go mod vendor` copies a dependency's source tree into the
   module's own `vendor/` directory, shipped as part of the module; older Go SDKs
   (pre-modules, e.g. `govendor`-era `qingcloud-sdk-go`) followed the same pattern
   manually ([QingCloud SDK example](https://github.com/1231sadqwf/qingcloud-sdk-go/blob/v2.0.0-alpha.38/docs/installation.md)).
   For JSON specifically this is largely moot in Go since `encoding/json` is
   stdlib — vendoring in Go SDKs is more commonly seen for HTTP/retry/crypto
   helper packages, not JSON.

3. **Feature-gated optional dependency (Rust).** Declaring a heavy dependency
   (e.g. `tokio`) as `optional = true` in `Cargo.toml` creates an implicit
   Cargo feature of the same name; consumers opt in via `features = ["tokio"]`,
   and the crate's own code is `#[cfg(feature = "tokio")]`-gated so a default
   build never pulls the dependency in
   ([Cargo optional-dependency pattern](https://users.rust-lang.org/t/optional-features/106138),
   [tokio::process docs](https://docs.rs/tokio/latest/tokio/process/index.html)).
   This is the standard idiom for Rust SDKs that want to support both sync-only
   and async-with-tokio consumers from one crate — it doesn't get you to zero
   deps, but it keeps the dependency opt-in rather than forced.

4. **Minimal embedded/hand-rolled parser.** Not confirmed for Stripe specifically
   in this pass — `stripe-go` uses stdlib `encoding/json` (so no hand-rolled
   parser needed there), and `stripe-java` depends on Gson rather than a
   hand-rolled parser ([stripe/stripe-java issue re: Gson necessity](https://github.com/stripe/stripe-java/issues/307)).
   No concrete example of a well-known SDK shipping a genuinely hand-rolled JSON
   parser was found in this pass — flagged as an open lead below rather than
   asserted.

---

## negative:

- No off-the-shelf generator was found that produces **both** zero-runtime-dep
  output **and** enforced (rather than merely typed) `oneOf`/`const` exclusivity —
  every generator surveyed either (a) doesn't enforce union exclusivity at all
  (quicktype, both TS and Swift targets share this defect) or (b) enforces it only
  by depending on a runtime library (Pydantic's validator, Jackson's
  `@JsonTypeInfo`).
- Rust and Java are **categorically** unable to reach zero-dep JSON handling from
  the standard library alone — neither ships a JSON facility in std/JDK, so "zero
  runtime third-party dependency" is unreachable regardless of generator choice;
  the practical floor is "one small, well-known dependency" (serde+serde_json;
  Jackson/Gson), sometimes hidden via shading.
- No generator surveyed confirmed automatic handling of JSON Schema's
  `type: [x, "null"]` nullable-vs-absent-vs-explicit-null three-way distinction
  out of the box; every language needs either a stdlib feature used correctly
  by hand (Swift's `decodeIfPresent`+`decodeNil()`, Python's PEP 655
  `Required[T | None]` vs `NotRequired[T]`) or a third-party wrapper type
  (Go's `oapi-codegen/nullable`, Rust's `serde_with::double_option`).
- "Async subprocess with zero deps" does not exist in Rust at all — not a
  generator gap, a language-level one (no stdlib executor).

## leads:

- go-jsonschema's exact import list for generated code (confirm zero
  third-party imports) and its enum-openness behavior were not confirmed from
  the README excerpt fetched — worth a direct run of the tool against a small
  schema and inspecting `go list -deps` on the output.
- typify's handling of `type: [x, "null"]` specifically (as opposed to plain
  optional properties) and whether it emits `#[serde(other)]` for open enums —
  not confirmed from the README; worth generating from a schema containing both
  and reading the emitted Rust.
- Whether any well-known SDK ships a genuinely hand-rolled/embedded JSON parser
  (item 4 of the fallback-practices ask) — not found in this pass; worth
  checking older/embedded-target SDKs (e.g. Arduino/microcontroller vendor SDKs,
  or pre-Jackson-era Java libraries) rather than mainstream cloud SDKs.
- quicktype's non-`--just-types` TypeScript `Convert`/validating mode's exact
  behavior on `oneOf`+`const` (does it reject a payload matching zero or two
  branches, or only zero?) — not directly tested here.

---

## Summary

| Language | Stdlib JSON? | Zero-dep generator? | Async stdlib? | Fallback practice |
|---|---|---|---|---|
| Python | Yes (`json`) | Yes — `datamodel-code-generator --output-model-type dataclasses.dataclass\|typing.TypedDict` | Yes (`asyncio.create_subprocess_exec`) | dacite/cattrs for dict→nested-dataclass convenience |
| TypeScript | Yes (`JSON.parse`) | Yes, but type-erased — `json-schema-to-typescript` / quicktype `--just-types` emit no runtime checks | Yes (`child_process`+`util.promisify`) | quicktype non-just-types mode for self-contained runtime validators |
| Rust | **No** | **No** — typify requires `serde`+`serde_json` at runtime | **No** (no stdlib async runtime at all) | Feature-gated optional `tokio`/`async-process` dep |
| Go | Yes (`encoding/json`) | Likely yes — `go-jsonschema` (imports unconfirmed in this pass) | Yes (goroutine + `os/exec`, no distinct "async") | Rarely needed for JSON; vendoring used for other deps |
| Swift | Yes-adjacent (`Codable`+Foundation, toolchain-bundled) | Yes for quicktype Swift/Codable target; **no** for `swift-openapi-generator` (needs `OpenAPIRuntime`) | No stdlib async subprocess yet — `swift-subprocess` is a separate package | Hand-wrap legacy `Foundation.Process` in `withCheckedContinuation` |
| Java | **No** | **No** — `jsonschema2pojo --annotationStyle none` removes annotations, not the runtime library requirement | Yes (`ProcessBuilder`+`onExit()` → `CompletableFuture`) | Shade/relocate the library into the SDK jar (AWS SDK v2 pattern) |
