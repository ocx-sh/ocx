# Attribute grammar

Each variant (or the struct) carries one `#[exit(...)]`, of exactly one form. An enum may put the form on
the type instead: it then applies to every variant that has none of its own.

| Form | `classify()` | `kind_detail()` |
|---|---|---|
| `Code, slug = "s", summary = "t"` | `Some(Code)` | `Fixed` row `s` |
| `defer(Code), slug = "s", summary = "t"` | `None` | `Fixed` row `s`, published under `Code` |
| `delegate` / `delegate = field` | the field's own `classify` | the field's own `kind_detail` |
| `chain, fallback(Code, slug = "s", summary = "t")` | `None` | `Chain` from `self`, falling back to row `s` |
| `chain = field, fallback(Code, slug = "s", summary = "t")` | `Some(Code)` | `Chain` from `field`, falling back to row `s` |
| `with = path, rows((Code, slug = "s", summary = "t"), ...)` | the picked row's answer | the picked row |

- `Code` is a variant name of `ocx_exit::ExitCode`; one that does not exist is a compile error.
- `delegate` is static: it calls the field's trait impls through `Box`/`Arc`, so a field whose type
  implements neither trait does not compile. `field` is a name or a tuple index, with `.field` steps
  for a payload's own field (`0.kind`). A struct's `#[exit(delegate = kind)]` also makes its `DETAILS`
  the field type's (the `T` of a `Box<T>` or `Arc<T>` field), so it takes no `reserve`. A delegating arm
  declares no row of its own.
- `chain` is the dynamic arm: only the binary can walk the cause chain, so the arm hands it the slug's
  fallback and the error to start from (`Detail::Chain`). Plain `chain` defers the exit code to the walker,
  so its fallback code is `Failure`;
  `chain = field` decides it too (the walker answers with the first classified cause of `field`) and is for
  a payload `source()` skips, such as an `#[error(transparent)]` one. `field` must itself be an error.
  Its direct `classify()` answers the fallback code only: the binary's chain resolution, not `classify()`,
  decides the exit status and slug, since only the binary can classify foreign causes such as `std::io::Error`.
- `with = path` is the escape hatch for a guard or a computed answer. `path` is
  `fn(&Self, [Row; N]) -> Pick<'_>` where `N` is the number of `rows(...)`; it returns `Pick::row(rows[i])`,
  `Pick::delegate(&inner)` or `Pick::chain(&error, rows[i])`, so it cannot answer with an undeclared slug.
  A `defer(Code)` row makes `Pick::row` answer `None`.
- Type level: `#[exit(family = "Name")]` overrides the `DetailEntry::family` string, the type's ident by default.
- Type level: `#[exit(reserve(Code, slug = "s", summary = "t"))]` lists a published row no variant answers with
  today, last in `DETAILS`; for a retired catch-all slug whose removal would be a wire change. Repeatable.

# Refused

Each is a compile error: a key given twice in one attribute, a second exit code in a row, a row coded
`Success`, `defer` inside `fallback(...)`, and a plain `chain` whose fallback is not `Failure`.

# Rows

`DETAILS` lists every declared row in arm order, then the type's reserved rows, deduplicated by slug. Declaring a slug again with another
code or summary is a compile error. A slug is unique to its code and summary within one type only.
