from __future__ import annotations

import json
import re
import time
from dataclasses import dataclass, field
from pathlib import Path

import pexpect

# Regex to strip ANSI escape sequences from captured output
_ANSI_RE = re.compile(r"\x1b\[[^a-zA-Z]*[a-zA-Z]|\x1b\][^\x07]*\x07")

# Matches ANSI wrapping around a line: leading sequences, content, trailing sequences.
# Used by realign_tables() to preserve styling after column reformatting.
_ANSI_WRAP_RE = re.compile(
    r"^((?:\x1b\[[^a-zA-Z]*[a-zA-Z])*)"     # leading ANSI (e.g. \x1b[4m)
    r"(.*?)"                                # content
    r"((?:\x1b\[[^a-zA-Z]*[a-zA-Z])*)$",    # trailing ANSI (e.g. \x1b[0m)
)

# Digest patterns for path truncation in recordings
# Object store sharded path: sha256/XXXXXXXX/YYYYYYYY/ZZZZZZZZZZZZZZZZ
_DIGEST_PATH_RE = re.compile(r"sha256/([a-f0-9]{8})/[a-f0-9]{8}/[a-f0-9]{16}")
# OCI reference digest: @sha256:64-hex-chars
_DIGEST_REF_RE = re.compile(r"@sha256:([a-f0-9]{8})[a-f0-9]{56}")

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

    def auto_height(self, padding: int = 2, minimum: int = 5) -> CastRecording:
        """Set height based on the actual number of lines in the recording."""
        max_y = 0
        y = 0
        for event in self.events:
            if event.event_type != "o":
                continue
            for char in event.data:
                if char == "\n":
                    y += 1
                    max_y = max(max_y, y)
        self.height = max(max_y + padding, minimum)
        return self

    def sanitize(self, replacements: dict[str, str]) -> CastRecording:
        """Replace literal strings in all event data (paths, registry names, etc.)."""
        self._merge_close_events()
        for event in self.events:
            for old, new in replacements.items():
                event.data = event.data.replace(old, new)
        return self

    def truncate_digests(self) -> CastRecording:
        """Shorten SHA256 digest strings for readable recordings.

        Replaces:
        - Object store paths: ``sha256/8ce298b2/f4c357ee/3a6257cf491742fc`` → ``sha256/8ce298b2..``
        - OCI reference digests: ``@sha256:8ce298b2f4c3...`` → ``@sha256:8ce298b2..``

        Merges close events first so digest patterns aren't split across PTY chunks.
        """
        self._merge_close_events()
        for event in self.events:
            event.data = _DIGEST_PATH_RE.sub(r"sha256/\1..", event.data)
            event.data = _DIGEST_REF_RE.sub(r"@sha256:\1..", event.data)
        return self

    def realign_tables(self) -> CastRecording:
        """Re-align table columns after content-shortening replacements.

        When sanitize/truncate_digests shorten cell content, the original
        column padding becomes excessive.  For each event whose data contains
        ``\\r\\n``-separated lines that all split into the same number of
        whitespace-delimited columns (>= 2), recalculate column widths.

        ANSI escape sequences (underline for headers, reverse for odd rows)
        are stripped before column detection and width measurement, then
        re-applied to the reformatted lines.
        """
        for event in self.events:
            # Separate optional ANSI "erase line" prefix from content
            erase_idx = event.data.find("\x1b[2K")
            if erase_idx >= 0:
                prefix = event.data[: erase_idx + 4]
                content = event.data[erase_idx + 4 :]
            else:
                prefix = ""
                content = event.data

            lines = content.split("\r\n")

            # Strip ANSI wrapping before splitting into columns so that
            # escape sequences don't pollute column counts or widths.
            parsed: list[tuple[int, list[str], str, str]] = []
            for i, line in enumerate(lines):
                m = _ANSI_WRAP_RE.match(line)
                if m:
                    lead, inner, trail = m.group(1), m.group(2), m.group(3)
                else:
                    lead, inner, trail = "", line, ""
                cols = inner.split()
                if cols:
                    parsed.append((i, cols, lead, trail))

            if len(parsed) < 2:
                continue
            col_counts = {len(cols) for _, cols, _, _ in parsed}
            if len(col_counts) != 1:
                continue
            ncols = col_counts.pop()
            if ncols < 2:
                continue

            widths = [0] * ncols
            for _, cols, _, _ in parsed:
                for j, cell in enumerate(cols):
                    widths[j] = max(widths[j], len(cell))

            new_lines = list(lines)
            for line_idx, cols, lead, trail in parsed:
                # Pad all columns when ANSI-wrapped (e.g. underlined header)
                # so the decoration extends to full table width.
                pad_last = bool(lead or trail)
                parts = [f"{cell:<{widths[j]}}" if (j < ncols - 1 or pad_last) else cell
                         for j, cell in enumerate(cols)]
                new_lines[line_idx] = lead + " ".join(parts) + trail

            event.data = prefix + "\r\n".join(new_lines)
        return self

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
        width: int = 100,
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

    def open(self) -> None:
        """Start a persistent interactive bash shell for recording."""
        self._shell = pexpect.spawn(
            "/bin/bash",
            ["--norc", "--noprofile"],
            env=self.env,
            dimensions=(self.height, self.width),
            timeout=60,
            encoding="utf-8",
        )
        self._shell.sendline("stty -echo")
        self._shell.sendline("bind 'set enable-bracketed-paste off' 2>/dev/null")
        self._shell.sendline(f'PS1="{self._SENTINEL}"')
        self._shell.expect_exact(self._SENTINEL)

    def close(self) -> None:
        """Close the persistent shell."""
        if self._shell is not None:
            self._shell.sendline("exit")
            self._shell.close()
            self._shell = None

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
