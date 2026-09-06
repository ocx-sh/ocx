# Terminal UIs

The split that keeps a TUI testable, the three doors terminal restore has to
close, what happens to registry text before it reaches a cell, and the
keybindings users already have muscle memory for. Loads with the Rust quality
rule whenever anything under a `tui/` module is in play.

Contents: [Architecture and the Event Loop](#architecture-and-the-event-loop) ·
[Terminal-State Guarantees](#terminal-state-guarantees) ·
[Untrusted Text](#untrusted-text) ·
[Interaction and Presentation](#interaction-and-presentation) ·
[Testing](#testing) · [What Agents Get Wrong](#what-agents-get-wrong-here) ·
[Sources](#sources)

**The mechanism** — a pure core the shell drives, one owner of raw mode,
sanitise at the display boundary, measure width in grapheme clusters — is
portable Rust TUI practice. The four calls below are **pinned decisions**,
already paid for, and not re-litigated in a PR.

- **TEA, with a harder purity guarantee than ratatui asks for.** ratatui
  explicitly declines to pick among TEA, Component and Flux; the choice here is
  TEA, and `state`/`render`/`event`/`tree` import no terminal at all.
- **`EventStream` + `tokio::select!` for the loop** — not a blocking poll on a
  spawned task, not a fixed frame budget. It is the only shape where an
  in-flight registry pull and a keypress are branches of the same `select!`.
- **No `insta`.** It is ratatui's official recipe; hand-written assertions state
  what must be true, survive a layout tweak, and can assert colour — snapshots
  cannot.
- **Strip U+200D even though it mangles emoji ZWJ sequences.** A package manager
  trades one glyph's fidelity for a predictable column budget.

Three neighbours, not duplicated here: the stdout/stderr split and TTY detection
are [cli-contract](cli-contract.md); escape-sequence neutralisation
of registry-sourced text is also [security.md](security.md); the runtime rules
the event loop obeys are [async.md](async.md).

## Architecture and the Event Loop

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TUI-01 | The pure core (`state`, `render`, `event`, `tree`) imports no `crossterm`, no `ratatui::Terminal`/backend, no `std::io`, no `tokio`, and does no I/O. Terminal ownership, raw mode and the runtime live in exactly one shell module. | `rg -n --glob '**/tui/{state,event,render,tree}.rs' -e 'crossterm::' -e 'use std::io' -e 'std::io::' -e 'tokio::' -e '\.await' .` — every hit is a finding; the patterns are anchored to imports and paths, so a bare word in a doc comment is not one | MUST |
| TUI-02 | Rendering is one pure `view(&Model, &mut Frame)`: no mutation outside render-local temporaries, no I/O. Same model, same frame, every time — that is what makes `TestBackend` deterministic. | Inspect the render entry point and its callees for `&mut` state parameters, `.await`, `std::fs`, `tokio::spawn` | MUST |
| TUI-03 | State changes happen only inside `handle`/`update`, which returns an action enum. The shell applies actions; it never assigns to model fields. | The shell module contains no `state.<field> =` and no `&mut self` setter calls outside the action-application match | MUST |
| TUI-04 | The event loop never blocks and never `.await`s I/O. Input, the tick, and every background result are branches of one `tokio::select!` (`crossbeam::Select` in a sync app); long operations are `tokio::spawn`ed with a channel back into the event enum — a registry pull awaited inline queues every keystroke, including the quit key, for its whole duration. | `rg -n --glob '**/tui/*.rs' --glob '!**/tui/{state,event,render,tree}.rs' -e '\.await' .` — every hit is a channel `recv`, a `select!` arm, or an `interval.tick()`; any network or filesystem await is a finding; the shell's await count is never zero, so restrict to added lines on a diff | MUST |
| TUI-05 | Never call blocking `crossterm::event::poll`/`event::read` inside an `async fn` — a 200 ms poll per tick parks a `multi_thread` worker. Use `EventStream`, or a dedicated `spawn_blocking` poll task re-armed in a loop. | `rg -n --type rust --glob '!external/**' -e 'event::poll' -e 'event::read' .` — any hit inside an `async fn` body is a finding | MUST |
| TUI-06 | Once the shell exceeds one screen's worth of concerns, split it **by feature** (event source, background scheduler, data loader, session lifecycle) — never by re-merging state/render/event, which destroys TUI-01. | Review: a shell module >2 kLOC with ≥3 unrelated concerns and repeated `(ctx, state, checker)` parameter triples | SHOULD |
| TUI-07 | Every `tokio::spawn`ed background task carries a generation token, and results whose generation is stale are discarded on receipt — the user can refresh or change scope while a fetch is in flight. | For each background result channel, the receiving arm compares a generation before applying | SHOULD |

## Terminal-State Guarantees

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TUI-08 | One RAII guard owns raw mode and the alternate screen, and one restore function is called from its `Drop`, from a `std::panic` hook installed **before** the first `enter()`, and from nowhere else. The hook restores, then re-invokes the previous hook. `Drop` alone loses the panic message: it prints into the alternate screen the `Drop` then tears down. | `rg -n --type rust --glob '!external/**' -e 'disable_raw_mode' -e 'LeaveAlternateScreen' .` — exactly one call site each, inside the guard, ignoring the guard's own `use` line; `rg -n --type rust --glob '!external/**' 'set_hook' .` — must exist (either spelling of the import) and must call the guard's restore | MUST |
| TUI-09 | No `std::process::exit` or `std::process::abort` reachable after the guard is entered — `exit` runs no destructors on any thread, leaving the user's shell without echo until they blind-type `reset`. Exit decisions return a value up to a `main` outside the guard's lifetime. Same rule as EXIT-02. | `rg -n --glob '**/tui/**' -e 'process::exit' -e 'process::abort' .` — any hit is a finding | MUST |
| TUI-10 | A caught `SIGINT`/`SIGTERM` handler does no terminal I/O; it sends a Quit event into the existing channel and lets the normal path run the guard. Signals do not unwind, so they skip `Drop` exactly like `exit()`. The reported status derives from the signal actually received — hardcoding 130 lies to systemd about SIGTERM. | `rg -n --type rust --glob '!external/**' -e 'signal_hook' -e 'ctrlc' -e 'signal::' .` — the handler body contains only a channel send | SHOULD |
| TUI-11 | Gate the TUI on `std::io::stdout().is_terminal()` **and** an explicit `--no-tui` flag, both falling through to the *same* plain, line-oriented code path — not to an error message. One implementation, three triggers: piped/CI, opt-out, and screen-reader users, whom an immediate-mode full repaint cannot serve at all. | `rg -n --type rust --glob '!external/**' -e 'is_terminal' -e 'enable_raw_mode' .` — the check precedes every raw-mode call. Then run the TUI entry point with stdout redirected to a file: with stdout not a TTY it writes usable plain output and exits 0, and `--no-tui` on a real TTY produces the same output | MUST |
| TUI-12 | Between guard entry and guard drop, nothing writes to stdout or stderr except `Terminal::draw`. Logging, progress bars and `eprintln!` are suppressed or buffered for the session — a stray write lands mid-frame and the diff cannot repair it. | `rg -n --glob '**/tui/**' -e 'println!' -e 'eprintln!' -e 'print!' -e 'ProgressBar' .` — any hit outside a test is a finding; confirm the tracing writer is disabled for the session | MUST |

```rust
// One restore. Three callers: Drop, the panic hook, and the signal path's
// Quit event — never a fourth, and never a second disable_raw_mode().
fn restore() -> io::Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)
}

// Installed BEFORE the first enter(), or the message paints into a screen
// Drop is about to tear down and the user sees a silent crash.
let previous = std::panic::take_hook();
std::panic::set_hook(Box::new(move |info| {
    let _ = restore();
    previous(info);
}));
```

## Untrusted Text

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TUI-13 | Every registry-, manifest- or network-sourced string passes the sanitiser at the *display* boundary. State and caches store the raw value; sanitisation is never persisted — sanitising on ingest corrupts the value used for matching and logging. | For each `Span`/`Line`/`Paragraph` constructed from a `Package`/`Manifest`/`Catalog` field, confirm a sanitiser call in between. `Line::raw(<registry field>)` with no sanitiser is a finding | MUST |
| TUI-14 | The sanitiser strips, in one pass: all C0/C1 controls, ESC-introduced CSI/OSC sequences, bidi overrides and isolates (U+202A–U+202E, U+2066–U+2069), and zero-width code points (U+200B–U+200D, U+FEFF). Styling is re-emitted from trusted code only. `strip-ansi-escapes` misses the bidi and zero-width classes, which are Unicode-content attacks; `ansi-to-tui` is a renderer, not a sanitiser — a well-formed OSC 52 is not "malformed". | One unit test per stripped class asserting absence from the output; a test asserting the function is not O(n²) | MUST |
| TUI-15 | Truncate for display by walking grapheme clusters (`unicode-segmentation`) and summing `unicode-width` per cluster, stopping before the column budget — never by byte slice or `chars().take(n)`, and only after sanitising. `unicode-width` is scalar-value width: `chars().count()` measures a CJK name at half its rendered width and can cut a combining sequence in half. | `rg -n --type rust --glob '!external/**' -e '\.chars\(\)\.take\(' -e '\.chars\(\)\.count\(\)' -e '&[a-z_]*\[\.\.' .` — each hit on non-literal text is a finding; byte slicing is never zero across a whole crate, so restrict to added lines on a diff | MUST |
| TUI-16 | A hand-written `Widget`/`StatefulWidget` impl intersects its `area` with `buf.area` before indexing into the buffer — an out-of-bounds write panics inside raw mode, where the message is eaten unless TUI-08 holds. | For every `impl Widget`/`StatefulWidget`, confirm an `intersection`/`clamp` precedes any `buf.set_*`/index | MUST |

## Interaction and Presentation

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TUI-17 | `Ctrl-C` quits from every mode, unconditionally, so the key mapper inspects `KeyEvent::modifiers` and not `KeyCode` alone. Raw mode disables the kernel's SIGINT translation — Ctrl-C arrives as an ordinary key event, and a mapper that drops modifiers silently rebinds the one key every terminal user trusts. | `rg -n --glob '**/tui/*.rs' -e 'key\.code' -e 'modifiers' .` — a file with `key.code` hits and no `modifiers` hit is a mapper matching on the code alone, and is a finding. Test: synthetic `Char('c') + CONTROL` in each mode returns Quit | MUST |
| TUI-18 | `Esc` cancels the current mode/overlay/filter and steps back one level; it never exits the process. `q` and `Ctrl-C` exit. | For each modal/overlay/input state, a test asserting `Esc` returns to the previous state | MUST |
| TUI-19 | Every key event handler filters on `key.kind == KeyEventKind::Press`. crossterm emits Press *and* Release per keypress on Windows only, so an unfiltered handler double-fires on a platform the developer is not testing on. | `rg -n --type rust --glob '!external/**' 'Event::Key\(' .` — each must have a `KeyEventKind` guard on the same path | MUST |
| TUI-20 | Every list/scroll surface accepts both arrow keys and vim keys (`j`/`k`, and `h`/`l` where lateral movement exists). One extra match arm; supporting one style is a free loss of half the audience. | For each `KeyCode::Up`/`Down` arm in a navigation context, confirm a `Char('k')`/`Char('j')` sibling | SHOULD |
| TUI-21 | Ship both a persistent context-relevant key-hint bar in the primary view and a full `?` help overlay. Neither alone. `?` for help and `/` for search are shared vocabulary — an invention costs a lookup every session. | Confirm a footer/hint widget in the default render path and a `?`-reachable overlay state | SHOULD |
| TUI-22 | Colour is never the only channel for meaning: every colour-coded state also carries a glyph, prefix or label — a TUI renders inside the user's palette, so no contrast ratio can be guaranteed. Semantic colours use terminal ANSI slots, never invented truecolor or hardcoded `Color::White`/`Color::Black`. Non-empty `NO_COLOR` degrades the TUI to an uncoloured render, overridable by an explicit `--color`. | `rg -n --glob '**/tui/**' -e 'Color::White' -e 'Color::Black' -e 'Color::Rgb' .` — each hit is a finding. Confirm the TUI's colour resolver consults the same `NO_COLOR` policy the CLI printer uses | MUST |
| TUI-23 | Feedback scales with duration: under ~1 s nothing, 1–10 s an indeterminate spinner, past ~10 s a determinate progress indicator **and** a cancel path. Never a modal that blocks input for a state the user did not ask to confirm. | Review each operation that can exceed 10 s (registry resolve, multi-artifact install) for a determinate indicator and a cancel key | SHOULD |
| TUI-24 | Handle `Event::Resize` as a normal event that re-clamps scroll offsets and redraws. State a minimum usable size and render a placeholder below it — ratatui has no minimum-size API, and `Constraint::Min`/`Percentage` degrade to zero-height areas silently, which reads as a broken app. | Test: drive the model to a 20×5 size and assert the render is a legible placeholder, not a panic and not an empty frame | SHOULD |
| TUI-25 | Leave mouse capture off by default. If enabled, make it runtime-togglable and document the Shift-to-select bypass — `EnableMouseCapture` takes away the terminal's native drag-select, and users copy package names and error text out of the UI. | `rg -n --type rust --glob '!external/**' 'EnableMouseCapture' .` — a hit with no toggle is a finding | CONSIDER |

## Testing

| ID | Rule | Verification | Severity |
|---|---|---|---|
| TUI-26 | Every screen and major render state has a `TestBackend` render test asserting content, at a narrow and a wide width. The render function is pure, so this is the cheapest test in the codebase. | `rg -n --type rust --glob '!external/**' 'TestBackend' .` — the count tracks the number of distinct screens; each test asserts on buffer content, not just that `draw` returned `Ok` | MUST |
| TUI-27 | Keybinding and state-transition tests call `handle`/`update` directly with synthetic input values and assert on model fields. They construct no `Terminal` — routing through a backend tests more machinery than the logic and hides which layer broke. | A test asserting keybinding behaviour that constructs a `Terminal` is a finding | MUST |
| TUI-28 | The sanitiser has a table-driven corpus with one case per attack class: CSI colour, cursor-move, OSC 8 hyperlink, OSC 52 clipboard write, U+202E bidi override, U+2066 isolate, zero-width joiner, BOM, a CJK string, a multi-codepoint emoji cluster. Each class fails differently, and a sanitiser handling CSI but not bidi passes any test written from a single example. | Each row asserts both the stripped output and the resulting display width | MUST |
| TUI-29 | A property test folds arbitrary sequences of the input alphabet through `handle` and asserts invariants that hold in every reachable state: selection index in range, scroll clamped to content, every mode reachable by `Esc` back to the root. | A `proptest!` over `Vec<TuiInput>` exists and asserts at least those three invariants | SHOULD |

## What Agents Get Wrong Here

1. **Awaiting a registry call inline in the event loop.** The model sees an
   `async fn` and an operation to perform, and writes `.await`. It compiles,
   works against a fast local registry, and freezes the UI for the whole
   round-trip in production. The most common defect in this topic by a wide margin.
2. **`chars().take(n)` for truncation.** The shortest obviously-correct-looking
   code, wrong for any non-ASCII string and doubly wrong on unsanitised text.
3. **`std::process::exit(1)` as the bail-out inside a TUI**, learned from CLI
   training data. Skips the guard, leaves the shell in raw mode, and nothing in
   the type system objects.
4. **Writing a `Drop` guard and stopping there**, treating panic-restore as
   covered because the guard exists. The hook is a separate mechanism and the
   failure is invisible in testing — the shell *is* restored; only the error
   message is lost.
5. **`Line::raw(desc)` on a registry string.** Nothing marks `desc: String` as
   untrusted, so no signal fires; agents sanitise the field the task names and
   no other, producing partial coverage that reads as done.
6. **Matching on `key.code` and ignoring `key.modifiers`.** The mapper looks
   complete and passes every test written from the plain keys — this is exactly
   how a Ctrl-C-does-not-quit bug gets in.
7. **Omitting the `KeyEventKind::Press` filter.** Nothing in the type signature
   hints at the Windows double-fire, and the developer's machine never
   reproduces it.
8. **Collapsing vim keys and arrow keys to one style**, because the duplicate
   match arms read as redundant to a model optimising for "clean".
9. **Skipping `is_terminal()` at the entry point** unless CI or piping is named
   in the task; the happy path is what gets written first.
10. **Belt-and-suspenders cleanup** — a second `disable_raw_mode()` "just to be
    safe" alongside the guard, turning one deterministic restore into two
    racing ones.
11. **Reaching for `insta` because it is the documented recipe**, adding a
    `.snap` corpus and a `cargo-insta` dependency where three explicit
    assertions would have said more.
12. **Hardcoding `Color::White`/`Color::Black`** rather than the terminal's
    default foreground, because dark-background terminals are what the training
    data assumes. Same reflex reaches for `WidgetRef`, which is gated behind
    `unstable-widget-ref` and documented as subject to change.

## Sources

- [ratatui: the Elm architecture](https://ratatui.rs/concepts/application-patterns/the-elm-architecture/) — the pure-`view` contract, and ratatui's refusal to pick a pattern
- [ratatui: panic hooks](https://ratatui.rs/recipes/apps/panic-hooks/) — the exact take-hook/restore/re-invoke shape TUI-08 requires
- [ratatui: FAQ](https://ratatui.rs/faq/) — Windows double key events, out-of-bounds buffer panics, and the "not an async library" stance
- [ratatui/templates](https://github.com/ratatui/templates) — the canonical spawned event task plus `tokio::select!` loop
- [`std::process::exit`](https://doc.rust-lang.org/std/process/fn.exit.html) — "no destructors on the current stack or any other thread's stack will be run"
- [CWE-150](https://cwe.mitre.org/data/definitions/150.html) — terminal escape injection, and it names LLM-generated output explicitly
- [trojansource.codes](https://trojansource.codes/) — CVE-2021-42574/42694 bidi and homoglyph mechanics; see also [CVE-2019-9535](https://nvd.nist.gov/vuln/detail/CVE-2019-9535), an iTerm2 RCE from terminal *output*
- [unicode-width](https://docs.rs/unicode-width/latest/unicode_width/) and [unicode-segmentation](https://docs.rs/unicode-segmentation/latest/unicode_segmentation/) — scalar-value width vs the grapheme-cluster iterator TUI-15 requires
