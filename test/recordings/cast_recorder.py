from __future__ import annotations

import json
import re
import time
from collections.abc import Callable
from dataclasses import dataclass, field
from pathlib import Path

import pexpect

from src.cast_screen import Screen, logical_lines

# Regex to strip ANSI escape sequences from captured output
_ANSI_RE = re.compile(r"\x1b\[[^a-zA-Z]*[a-zA-Z]|\x1b\][^\x07]*\x07")


# Digest patterns for path truncation in recordings
# Object store sharded path: sha256/XXXXXXXX/YYYYYYYY/ZZZZZZZZZZZZZZZZ
_DIGEST_PATH_RE = re.compile(r"sha256/([a-f0-9]{8})/[a-f0-9]{8}/[a-f0-9]{16}")
# OCI reference digest: @sha256:64-hex-chars. The decorated-table renderer
# paints `@` (punct) and `sha256:…` (digest) as separate SGR spans, so a
# reset+colour escape run can sit between `@` and `sha256:`. Capture that
# run and re-emit it verbatim so the recording stays coloured and shortened.
_DIGEST_REF_RE = re.compile(r"@((?:\x1b\[[0-9;]*m)*)sha256:([a-f0-9]{8})[a-f0-9]{56}")
# Bare digest in its own column / tree annotation (no `@`, no `/`): the
# binary prints the full hash everywhere; the recorder is the single
# truncation point for compact docs. Run *after* the path/ref subs so an
# already-shortened digest (8 hex) can't re-match. Any surrounding colour
# SGR sits outside the match and is left intact.
_DIGEST_BARE_RE = re.compile(r"sha256:([a-f0-9]{8})[a-f0-9]{56}(?![a-f0-9])")

# The unique-repo prefix every acceptance test puts in front of a repository
# name (`unique_repo`, `t_<8 hex>_`). No recording may show one, whether the
# display map knows that repository or not (an index query prints it bare, in
# quotes, inside a path or after a registry host).
_RUN_PREFIX_RE = re.compile(r"(?<![A-Za-z0-9])t_[0-9a-f]{8}_")

# Random per-run id of a download or staging directory.
_TEMP_ID_RE = re.compile(r"/temp/([0-9a-f]{8})[0-9a-f]{24}(?![0-9a-f])")

# A single SGR escape (`\x1b[…m`). Realign only ever reflows styled table
# output, which uses SGR colour codes exclusively.
_SGR_RE = re.compile(r"\x1b\[[0-9;]*m")
# Inter-column boundary in the *visible* text: the renderer pads a cell
# then adds a two-space GAP, so columns are always separated by a run of
# two-or-more spaces. Cell values never contain a double space (digests,
# identifiers, tags, visibility), so this split is unambiguous.
_GAP_RE = re.compile(r"\x20{2,}")
# Control residue a progress spinner leaves *after* its final `\x1b[2K`:
# cursor movement (CSI with any final byte except `m`, which is SGR and is
# real styling) and bare carriage returns.  Anchored at the start of the
# post-erase content only — an interior `\r\n` is a line break, not residue.
_CTRL_RESIDUE_RE = re.compile(r"(?:\x1b\[[0-9;]*[@-ln-~]|\r)+")

_GAP = "  "
_UNDERLINE = "\x1b[4m"  # marks the (bold+underlined) header row
_DIM = "\x1b[2m"        # zebra wrap on odd data rows
_RESET = "\x1b[0m"

_PAD_BEFORE_CR_RE = re.compile(r" +(?=\r)")

# From the start of the first line holding a braille spinner glyph up to the final
# erase-line before real output. Lines before it are kept: a spinner shares a PTY
# chunk with the text that precedes it.
_PROGRESS_PREFIX_RE = re.compile(
    r"(?:^|(?<=\n))(?=[^\n]*[\u2800-\u28FF])"  # a line-start whose line has a glyph
    r"(.*\r\x1b\[2K)"                           # greedy: last \r\x1b[2K after it
    r"(?=\x1b\[[\d;]*m|[^\x1b\r])",              # followed by ANSI style or visible char
    re.DOTALL,
)

def _sanitize_passes(replacements: dict[str, str]) -> list[tuple[re.Pattern[str], str]]:
    """Display map first (literal, in order), then any test-run prefix the map did not know."""
    literal = [(re.compile(re.escape(old)), new.replace("\\", "\\\\")) for old, new in replacements.items()]
    return [*literal, (_RUN_PREFIX_RE, "")]


_DIGEST_PASSES: list[tuple[re.Pattern[str], str]] = [
    (_DIGEST_PATH_RE, r"sha256/\1.."),
    (_DIGEST_REF_RE, r"@\1sha256:\2.."),
    (_DIGEST_BARE_RE, r"sha256:\1.."),
    (_TEMP_ID_RE, r"/temp/\1.."),
]


def _apply(text: str, passes: list[tuple[re.Pattern[str], str]]) -> str:
    for pattern, repl in passes:
        text = pattern.sub(repl, text)
    return text


def sanitize_text(text: str, replacements: dict[str, str]) -> str:
    """Apply the display map, then drop any test-run prefix the map did not know."""
    return _apply(text, _sanitize_passes(replacements))


def truncate_digests_text(text: str) -> str:
    """Shorten every digest form to its first 8 hex characters."""
    return _apply(text, _DIGEST_PASSES)


def _substitute_stream(chunks: list[str], pattern: re.Pattern[str], repl: str) -> list[str]:
    """``pattern.sub`` over the concatenation of *chunks*, keeping the chunk boundaries.

    A PTY read can end in the middle of a word, so a match may straddle two
    events; matching each event alone then misses it. The replacement goes to
    the chunk the match started in and the rest of the match is removed from
    the chunks it ran into.
    """
    ends = []
    total = 0
    for chunk in chunks:
        total += len(chunk)
        ends.append(total)
    combined = "".join(chunks)
    out: list[list[str]] = [[] for _ in chunks]

    def owner(pos: int) -> int:
        return next(i for i, end in enumerate(ends) if pos < end)

    def copy(a: int, b: int) -> None:
        while a < b:
            i = owner(a)
            stop = min(b, ends[i])
            out[i].append(combined[a:stop])
            a = stop

    cursor = 0
    for m in pattern.finditer(combined):
        copy(cursor, m.start())
        out[owner(m.start())].append(m.expand(repl))
        cursor = m.end()
    copy(cursor, len(combined))
    return ["".join(parts) for parts in out]


@dataclass
class CastEvent:
    timestamp: float
    event_type: str
    data: str


@dataclass
class CastRecording:
    width: int = 100
    height: int = 24
    title: str = ""
    events: list[CastEvent] = field(default_factory=list)

    def to_cast(self) -> str:
        header = json.dumps({
            "version": 2,
            "width": self.width,
            "height": self.height,
            "title": self.title,
        })
        lines = [header]
        for event in self.events:
            lines.append(json.dumps([
                round(event.timestamp, 3),
                event.event_type,
                event.data,
            ]))
        return "\n".join(lines) + "\n"

    def fit_width(self, minimum: int = 100, cap: int = 120) -> CastRecording:
        """Pick the frame width in [minimum, cap] that wraps the least.

        The player scales the frame to the page, so a wider frame means a
        smaller font; a line over the cap wraps instead. Progress redraws move
        the cursor by rows, so a width other than the PTY's can redraw over text
        the PTY kept: only widths whose screen has the same lines count. Fewest
        wrapped rows, then fewest wrapped characters, then the narrower width.
        """

        def replay(width: int) -> Screen:
            screen = Screen(width, 10**6)
            for event in self.events:
                if event.event_type == "o":
                    screen.feed(event.data)
            return screen

        reference = logical_lines(replay(self.width))
        best: tuple[int, int, int] | None = None
        for width in range(minimum, cap + 1):
            screen = replay(width)
            if logical_lines(screen) != reference:
                continue
            wrapped = [r for r in screen.rows if r.wrapped and r.text()]
            score = (len(wrapped), sum(len(r.text()) for r in wrapped), width)
            best = min(best or score, score)
        self.width = best[2] if best else self.width
        return self

    def auto_height(self, padding: int = 2, minimum: int = 5) -> CastRecording:
        """Set height from the rows the recording occupies at its width.

        A line longer than the width wraps onto a further row. Counting only
        newlines left the frame too short, the terminal scrolled, and the
        first command of the cast was cut to its tail.
        """
        screen = Screen(self.width, 10**6)
        for event in self.events:
            if event.event_type == "o":
                screen.feed(event.data)
        self.height = max(screen.max_row + padding, minimum)
        return self

    def sanitize(self, replacements: dict[str, str]) -> CastRecording:
        """Replace literal strings in all event data (paths, registry names, etc.)."""
        self._merge_close_events()
        self._substitute(_sanitize_passes(replacements))
        return self

    def strip_progress(self) -> CastRecording:
        """Remove progress spinner artifacts from output events.

        The tracing-indicatif spinner uses ``\\r\\x1b[2K`` (carriage-return +
        erase-line) to overwrite itself, and cursor-up/down sequences for
        multi-line progress bars.  When the command completes, the spinner is
        erased and the real output (table, tree, etc.) follows on the same line.

        Terminal emulators faithfully replay the cursor movement, which can
        leave ghost empty lines in the rendered GIF.  This method strips
        everything before the last erase-line sequence in each event, so only
        the clean output remains.
        """
        self._merge_close_events()
        for event in self.events:
            event.data = _PROGRESS_PREFIX_RE.sub("", event.data)
        # A bar pads itself to the PTY width; those blanks would wrap in any narrower frame.
        self._substitute([(_PAD_BEFORE_CR_RE, "")])
        return self

    def truncate_digests(self) -> CastRecording:
        """Shorten SHA256 digest strings for readable recordings.

        Replaces:
        - Object store paths: ``sha256/8ce298b2/f4c357ee/3a6257cf491742fc`` → ``sha256/8ce298b2..``
        - OCI reference digests: ``@sha256:8ce298b2f4c3...`` → ``@sha256:8ce298b2..``,
          including the coloured form where SGR escapes split ``@`` from ``sha256:``.
        - Bare column / annotation digests: ``sha256:8ce298b2f4c3...`` → ``sha256:8ce298b2..``.

        Order matters: path and ref subs run first; the bare sub then catches
        the standalone column form without re-matching an already-shortened
        digest.

        Merges close events first so digest patterns aren't split across PTY chunks.
        """
        self._merge_close_events()
        self._substitute(_DIGEST_PASSES)
        return self

    def realign_tables(
        self, shorten: Callable[[str], str] = lambda cell: cell
    ) -> CastRecording:
        """Re-align decorated tables after content-shortening replacements.

        ``truncate_digests`` / ``sanitize`` shrink cell content *after* the
        binary already padded every column to the longest (untruncated)
        value, leaving over-wide gaps.

        The table style has no vertical ``│`` and no rule line: the header
        row is bold+underlined (``\\x1b[4m``) and every cell — plus the
        two-space inter-column GAP — is padded; data rows space-align under
        it, odd rows dim-wrapped (zebra). A block is a header line (carries
        the underline SGR, splits into ≥2 columns) followed by ≥1 data rows
        that split into the same column count. Columns are recomputed from
        the trimmed visible content and re-padded; each cell's raw ANSI span
        (its colour, multi-part identifier ink, and the odd-row dim wrap) is
        sliced out verbatim — only the trailing pad is resized. The header's
        underline is re-extended across the GAP so it stays one line; the
        zebra dim is re-applied across the GAP on odd rows.
        """
        # `shorten` runs on each cell here, before sanitize/truncate, so a cell
        # is found by the header's column starts while the binary's padding is
        # intact; an empty cell then cannot shift its neighbours.
        self._merge_close_events()

        def analyze(line: str) -> tuple[str, list[int], list[tuple[int, int]]]:
            """Return (visible, vmap, escs): visible text, the raw index of
            each visible char, and (start, end) spans of every SGR escape."""
            escs: list[tuple[int, int]] = []
            vmap: list[int] = []
            pos = 0
            while pos < len(line):
                m = _SGR_RE.match(line, pos)
                if m:
                    escs.append((m.start(), m.end()))
                    pos = m.end()
                    continue
                vmap.append(pos)
                pos += 1
            vmap.append(len(line))
            visible = "".join(line[v] for v in vmap[:-1])
            return visible, vmap, escs

        def spans(visible: str) -> list[tuple[int, int]]:
            """Content spans (no surrounding pad), split on the ≥2-space GAP."""
            out: list[tuple[int, int]] = []
            prev = 0
            for m in _GAP_RE.finditer(visible):
                if m.start() > prev:
                    out.append((prev, m.start()))
                prev = m.end()
            if len(visible) > prev:
                out.append((prev, len(visible)))
            trimmed: list[tuple[int, int]] = []
            for st, en in out:
                seg = visible[st:en].rstrip()
                if seg:
                    trimmed.append((st, st + len(seg)))
            return trimmed

        for event in self.events:
            erase_idx = event.data.rfind("\x1b[2K")
            if erase_idx >= 0:
                prefix = event.data[: erase_idx + 4]
                content = event.data[erase_idx + 4 :]
            else:
                prefix = ""
                content = event.data

            # A spinner's teardown does not stop at its last erase: the real
            # tail is `…\x1b[2K\x1b[1A` (or a bare `\r`), and whatever follows
            # the erase leads the first header cell.  `analyze` only knows SGR
            # (`…m`), so a cursor-movement escape counts as four *visible*
            # characters — column 0 measures four wider than it renders, and
            # every data row under the header is padded to that phantom width.
            # Move the residue into the prefix, which is re-emitted verbatim.
            residue = _CTRL_RESIDUE_RE.match(content)
            if residue:
                prefix += content[: residue.end()]
                content = content[residue.end() :]

            lines = content.split("\r\n")
            new_lines = list(lines)
            changed = False

            i = 0
            while i < len(lines):
                if _UNDERLINE not in lines[i]:
                    i += 1
                    continue
                hv, hmap, hesc = analyze(lines[i])
                hsp = spans(hv)
                if len(hsp) < 2:
                    i += 1
                    continue
                ncols = len(hsp)
                block = [(i, lines[i], hmap, hesc, hsp)]
                j = i + 1
                while j < len(lines):
                    dv, dmap, desc = analyze(lines[j])
                    if not dv.strip():
                        break
                    if _UNDERLINE in lines[j]:
                        break  # a new header, or a footnote: not a row
                    dsp = spans(dv)
                    if len(dsp) != ncols:
                        # An empty cell leaves a wider gap, not a missing one:
                        # place each cell under the header column it starts in.
                        starts = [hs for hs, _ in hsp]
                        # A cell fits the header column it starts in; a longer
                        # run is a sentence under the table, not a row.
                        edges = [*starts[1:], 10**9]
                        if not dsp or any(
                            ds not in starts or de - ds > edges[starts.index(ds)] - ds - len(_GAP)
                            for ds, de in dsp
                        ):
                            break
                        cells: list[tuple[int, int] | None] = [None] * ncols
                        for sp_ in dsp:
                            cells[starts.index(sp_[0])] = sp_
                        dsp = cells
                    block.append((j, lines[j], dmap, desc, dsp))
                    j += 1
                if len(block) < 2:
                    i += 1
                    continue

                widths = [0] * ncols
                for _, raw, vmap, escs, sp in block:
                    for c, span in enumerate(sp):
                        if span is not None:
                            cell = shorten(self._cell_raw(raw, vmap, escs, span))
                            widths[c] = max(widths[c], len(_SGR_RE.sub("", cell)))

                for rrow, (idx, raw, vmap, escs, sp) in enumerate(block):
                    is_header = rrow == 0
                    # Data rows are 0-based after the header; odd ones are
                    # dim-wrapped (zebra) by the renderer.
                    is_zebra = (not is_header) and (rrow - 1) % 2 == 1
                    lead_first = ""
                    parts: list[str] = []
                    for c, span in enumerate(sp):
                        if span is None:
                            parts.append(" " * widths[c])
                            continue
                        cell = shorten(self._cell_raw(raw, vmap, escs, span))
                        if c == 0 and is_header:
                            lead_first = self._cell_lead(raw, vmap, escs, span)
                        pad_n = widths[c] - len(_SGR_RE.sub("", cell))
                        if is_header:
                            # Keep the pad underlined too, else the header
                            # underline breaks under each column's padding.
                            pad = lead_first + " " * pad_n + _RESET
                        else:
                            pad = " " * pad_n
                        parts.append(cell + pad)
                    if is_header:
                        gap = lead_first + _GAP + _RESET
                    elif is_zebra:
                        gap = _DIM + _GAP + _RESET
                    else:
                        gap = _GAP
                    new_lines[idx] = gap.join(parts)
                    changed = True

                i = j

            if changed:
                event.data = prefix + "\r\n".join(new_lines)
        return self

    def _substitute(self, passes: list[tuple[re.Pattern[str], str]]) -> None:
        outputs = [e for e in self.events if e.event_type == "o"]
        for pattern, repl in passes:
            for event, data in zip(outputs, _substitute_stream([e.data for e in outputs], pattern, repl)):
                event.data = data

    @staticmethod
    def _cell_bounds(
        vmap: list[int], escs: list[tuple[int, int]], span: tuple[int, int]
    ) -> tuple[int, int, int]:
        """Raw ``(lead_start, content_start, trail_end)`` of a visible span.

        Absorbs the ANSI directly bracketing the content so colour / dim /
        underline is preserved; never a space, so multi-part colour stays
        intact.
        """
        s, e = span
        cs = vmap[s]
        ls = cs
        while any(en == ls for _, en in escs):
            ls = next(st for st, en in escs if en == ls)
        te = vmap[e - 1] + 1
        while any(st == te for st, _ in escs):
            te = next(en for st, en in escs if st == te)
        return ls, cs, te

    @classmethod
    def _cell_raw(cls, raw, vmap, escs, span) -> str:
        ls, _, te = cls._cell_bounds(vmap, escs, span)
        return raw[ls:te]

    @classmethod
    def _cell_lead(cls, raw, vmap, escs, span) -> str:
        ls, cs, _ = cls._cell_bounds(vmap, escs, span)
        return raw[ls:cs]

    def _merge_close_events(self, threshold: float = 0.05) -> None:
        """Merge consecutive output events within *threshold* seconds."""
        if not self.events:
            return
        merged: list[CastEvent] = [self.events[0]]
        for event in self.events[1:]:
            prev = merged[-1]
            if (
                event.event_type == prev.event_type
                and event.timestamp - prev.timestamp < threshold
            ):
                prev.data += event.data
            else:
                merged.append(event)
        self.events = merged

    def write(self, path: Path) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(self.to_cast())


class CastRecorder:
    """Records CLI interactions as asciicast v2 files.

    Spawns a persistent bash shell through a PTY so that child processes
    (like ocx) inherit a real terminal environment with proper signal
    handling and TTY detection.

    Typing uses simulated timing (deterministic).
    Command output uses real-time capture (so progress bars animate).
    """

    _SENTINEL = "___CAST_PROMPT_a7b3c9___"

    def __init__(
        self,
        env: dict[str, str],
        *,
        width: int = 120,
        height: int = 24,
        prompt: str = "$ ",
        typing_delay: float = 0.04,
        inter_command_delay: float = 1.0,
        output_delay: float = 0.3,
        end_pause: float = 2.0,
    ):
        self.env = env
        self.width = width
        self.height = height
        self.prompt = prompt
        self.typing_delay = typing_delay
        self.inter_command_delay = inter_command_delay
        self.output_delay = output_delay
        self.end_pause = end_pause
        self._events: list[CastEvent] = []
        self._clock: float = 0.0
        self._shell: pexpect.spawn | None = None

    def open(self, *, shell: str = "bash") -> None:
        """Start a persistent interactive shell for recording.

        ``shell`` is the login-shell binary to spawn, resolved via ``PATH``
        (WP-16b, ``# shell:`` doc-script header key, default ``"bash"``).
        Only the bash arm is exercised today — every shipped ``cast: true``
        script uses the default — but the parameter lets a future non-bash
        cast select an interpreter without a second recorder pipeline
        (``design_spec_doc_command_scripts.md`` §6i: one tree, one
        discovery path). The ``--norc``/``--noprofile``/PS1-sentinel dance
        below is bash-specific; a non-bash shell would need its own
        no-rcfile flags and sentinel-prompt incantation here.
        """
        self._shell = pexpect.spawn(
            shell,
            ["--norc", "--noprofile"],
            env=self.env,
            dimensions=(self.height, self.width),
            timeout=60,
            encoding="utf-8",
        )
        self._shell.sendline("stty -echo")
        self._shell.sendline("bind 'set enable-bracketed-paste off' 2>/dev/null")
        # The assignment is split across two adjacent quoted strings — bash
        # concatenates them, so PS1 still ends up as the sentinel exactly, but
        # the *line* never contains it.  It has to: these three lines are sent
        # in one burst, so when the shell is slow enough to start that `stty
        # -echo` has not run yet, readline echoes the queued lines itself. A
        # line spelling the sentinel out would then put a sentinel in the
        # output that is not a prompt, `expect_exact` below would match that
        # echo, and the real prompt would stay unconsumed — leaving every
        # later `_read_until_prompt` one command behind for the rest of the
        # session (the recording then attributes each command's output to its
        # predecessor).  Load-dependent, so it surfaces as a flaky recording.
        head, tail = self._SENTINEL[:8], self._SENTINEL[8:]
        self._shell.sendline(f'PS1="{head}""{tail}"')
        self._shell.expect_exact(self._SENTINEL)

    def close(self) -> None:
        """Close the persistent shell."""
        if self._shell is not None:
            self._shell.sendline("exit")
            self._shell.close()
            self._shell = None

    def silent_setup(self, command: str, *, timeout: int = 10) -> None:
        """Run *command* in the shell without emitting events to the cast.

        Used for pre-flight setup (e.g. ``cd`` into a publisher work directory)
        that should not appear in the recorded session.
        """
        assert self._shell is not None, "call open() before silent_setup()"
        self._shell.sendline(command)
        self._shell.expect_exact(self._SENTINEL, timeout=timeout)

    def _emit(self, data: str) -> None:
        self._events.append(CastEvent(
            timestamp=self._clock,
            event_type="o",
            data=data,
        ))

    def type_command(self, command: str) -> None:
        """Simulate typing a command character by character."""
        self._emit(self.prompt)
        for char in command:
            self._clock += self.typing_delay
            self._emit(char)
        self._clock += self.typing_delay
        self._emit("\r\n")

    def _read_until_prompt(
        self, timeout: int = 60, *, emit: bool = True,
    ) -> str:
        """Read real-time output from the shell until the prompt sentinel appears."""
        assert self._shell is not None
        sentinel = self._SENTINEL
        sentinel_len = len(sentinel)
        buffer = ""
        emitted_up_to = 0
        wall_start = time.monotonic()
        clock_base = self._clock + self.output_delay

        while True:
            try:
                chunk = self._shell.read_nonblocking(size=4096, timeout=0.1)
                if chunk:
                    elapsed = time.monotonic() - wall_start
                    self._clock = clock_base + elapsed
                    buffer += chunk

                    idx = buffer.find(sentinel)
                    if idx >= 0:
                        remaining = buffer[emitted_up_to:idx]
                        if remaining and emit:
                            self._emit(remaining)
                        return buffer[:idx]

                    safe_end = len(buffer) - sentinel_len
                    if safe_end > emitted_up_to and emit:
                        to_emit = buffer[emitted_up_to:safe_end]
                        self._emit(to_emit)
                        emitted_up_to = safe_end

            except pexpect.TIMEOUT:
                if time.monotonic() - wall_start > timeout:
                    raise TimeoutError(
                        f"Command timed out after {timeout}s. "
                        f"Buffer so far: {buffer!r}"
                    )
            except pexpect.EOF:
                remaining = buffer[emitted_up_to:]
                if remaining and emit:
                    self._emit(remaining)
                return buffer

    def run_command(
        self,
        display_cmd: str,
        actual_cmd: str,
        *,
        timeout: int = 60,
    ) -> str:
        """Type and execute a command in the persistent shell.

        Returns the captured output.  Raises AssertionError on non-zero exit.

        A ``PROMPT_COMMAND``/``precmd`` hook registered inside the recorded
        script (e.g. ``eval "$(ocx self activate --shell=bash)"`` inside a
        ``# region cast`` block) fires **twice** per call: once when the
        sentinel prompt reappears after *actual_cmd* returns, and a second
        time for the silent ``echo $?`` exit-code probe below. Harmless for
        an idempotent hook (WP-16b) — the per-prompt reconciler is exactly
        that, a no-op re-apply when nothing changed — but worth knowing
        before debugging an unexpectedly-doubled side effect in a cast.
        """
        assert self._shell is not None, "call open() before run_command()"

        self.type_command(display_cmd)
        self._shell.sendline(actual_cmd)
        output = self._read_until_prompt(timeout)

        # Check exit code silently
        saved_clock = self._clock
        self._shell.sendline("echo $?")
        rc_output = self._read_until_prompt(5, emit=False)
        self._clock = saved_clock

        rc_str = _ANSI_RE.sub("", rc_output).strip()
        if rc_str and rc_str != "0":
            raise AssertionError(
                f"Command failed (rc={rc_str}): {actual_cmd}\n"
                f"Output: {output}"
            )

        self._clock += self.inter_command_delay
        return output

    def pause(self, seconds: float) -> None:
        self._clock += seconds

    def build(self, title: str = "") -> CastRecording:
        # Add a final empty event so the player holds the last frame visible
        events = list(self._events)
        self._clock += self.end_pause
        events.append(CastEvent(self._clock, "o", ""))

        return CastRecording(
            width=self.width,
            height=self.height,
            title=title,
            events=events,
        )
