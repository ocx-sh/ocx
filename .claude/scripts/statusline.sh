#!/usr/bin/env bash
# Claude Code status line — robbyrussell-inspired, cross-platform (Linux/macOS/WSL).
# Reads JSON from stdin (provided by Claude Code), renders 1-2 colored lines.

set -uo pipefail
set -f

input=$(cat)
if [ -z "$input" ]; then
    printf "Claude"
    exit 0
fi

CYAN='\033[0;36m'
BLUE='\033[1;34m'
RED='\033[0;31m'
YELLOW='\033[0;33m'
GREEN='\033[0;32m'
DIM='\033[2m'
RESET='\033[0m'

# GNU-first, BSD-fallback date helper. Silent on failure.
# Input is Unix epoch seconds (integer).
format_time() {
    local epoch="$1" style="${2:-time}" fmt
    case "$style" in
        time) fmt='+%-l:%M%p' ;;
        datetime) fmt='+%b %-d, %-l:%M%p' ;;
        *) fmt='+%b %-d' ;;
    esac
    date -d "@$epoch" "$fmt" 2>/dev/null ||
        date -r "$epoch" "$fmt" 2>/dev/null ||
        return 1
}

# Single jq call → unit-separated → bash vars. Unit separator (US, \x1f) avoids
# IFS-whitespace collapsing that would happen with tabs/spaces on empty fields.
IFS=$'\x1f' read -r cwd model ctx_pct_raw duration_ms fh_pct fh_reset sd_pct sd_reset <<EOF
$(printf '%s' "$input" | jq -r '[
  .workspace.current_dir // .cwd // "",
  .model.display_name // "",
  .context_window.used_percentage // "",
  .cost.total_duration_ms // 0,
  .rate_limits.five_hour.used_percentage // "",
  .rate_limits.five_hour.resets_at // "",
  .rate_limits.seven_day.used_percentage // "",
  .rate_limits.seven_day.resets_at // ""
] | map(tostring) | join("\u001f")' 2>/dev/null)
EOF

# Visual width of a colored string: render escapes, strip CSI sequences,
# then count characters. Approximation — wide emoji count as 1.
visual_len() {
    local s
    s=$(printf '%b' "$1" | sed $'s/\x1b\\[[0-9;]*m//g')
    printf '%s' "${#s}"
}

dir=$(basename "${cwd:-.}")

branch=""
dirty=""
if [ -n "${cwd:-}" ] && [ -d "$cwd" ]; then
    branch=$(GIT_OPTIONAL_LOCKS=0 git -C "$cwd" symbolic-ref --short HEAD 2>/dev/null ||
        GIT_OPTIONAL_LOCKS=0 git -C "$cwd" rev-parse --short HEAD 2>/dev/null) || branch=""
    if [ -n "$branch" ]; then
        if [ -n "$(GIT_OPTIONAL_LOCKS=0 git -C "$cwd" --no-optional-locks status --porcelain 2>/dev/null)" ]; then
            dirty="*"
        fi
    fi
fi

# Context %: Claude Code pre-computes this; may be null early in the session.
ctx_pct=""
if [ -n "${ctx_pct_raw:-}" ]; then
    ctx_pct=$(printf '%.0f' "$ctx_pct_raw" 2>/dev/null || echo "")
fi

# Bold context bar: traffic-light color, full block glyphs.
build_bar() {
    local pct="$1" width="${2:-10}" color filled empty bar i
    if [ "$pct" -ge 80 ]; then
        color="$RED"
    elif [ "$pct" -ge 50 ]; then
        color="$YELLOW"
    else
        color="$GREEN"
    fi
    filled=$((pct * width / 100))
    [ "$filled" -gt "$width" ] && filled="$width"
    empty=$((width - filled))
    bar=""
    i=0
    while [ "$i" -lt "$filled" ]; do
        bar="${bar}█"
        i=$((i + 1))
    done
    i=0
    while [ "$i" -lt "$empty" ]; do
        bar="${bar}░"
        i=$((i + 1))
    done
    printf '%b%s%b' "$color" "$bar" "$RESET"
}

# Subtle bar for rate-limit lines: thin horizontal rules, always dim, no color
# thresholds — supplementary info, not an alarm. Switches to YELLOW/RED only
# when usage crosses the danger zone, so a real cap exhaustion still pops.
build_subtle_bar() {
    local pct="$1" width="${2:-10}" color filled empty bar i
    if [ "$pct" -ge 90 ]; then
        color="$RED"
    elif [ "$pct" -ge 75 ]; then
        color="$YELLOW"
    else
        color="$DIM"
    fi
    filled=$((pct * width / 100))
    [ "$filled" -gt "$width" ] && filled="$width"
    empty=$((width - filled))
    bar=""
    i=0
    while [ "$i" -lt "$filled" ]; do
        bar="${bar}━"
        i=$((i + 1))
    done
    i=0
    while [ "$i" -lt "$empty" ]; do
        bar="${bar}─"
        i=$((i + 1))
    done
    printf '%b%s%b' "$color" "$bar" "$RESET"
}

# Session duration from cost.total_duration_ms (milliseconds).
duration=""
if [ -n "${duration_ms:-}" ] && [ "${duration_ms:-0}" != "0" ]; then
    elapsed=$((duration_ms / 1000))
    if [ "$elapsed" -lt 60 ]; then
        duration="${elapsed}s"
    elif [ "$elapsed" -lt 3600 ]; then
        duration="$((elapsed / 60))m"
    else
        duration="$((elapsed / 3600))h$(((elapsed % 3600) / 60))m"
    fi
fi

# Line 1
line1="${CYAN}${dir}${RESET}"
if [ -n "$branch" ]; then
    if [ -n "$dirty" ]; then
        line1="${line1} ${BLUE}git:(${RED}${branch}${RED}${dirty}${BLUE})${RESET}"
    else
        line1="${line1} ${BLUE}git:(${RED}${branch}${BLUE})${RESET}"
    fi
fi
if [ -n "$model" ]; then
    line1="${line1}  ${model}"
fi
if [ -n "$ctx_pct" ]; then
    bar=$(build_bar "$ctx_pct" 10)
    line1="${line1} ${bar} ${ctx_pct}%"
fi
if [ -n "$duration" ]; then
    line1="${line1} ${DIM}⏱ ${duration}${RESET}"
fi

# Build rate-limit candidates at progressively shorter widths so the segment
# gracefully shrinks as available space decreases. Ordered fullest → tersest.
# Empty strings mean "no rate-limit data" — nothing to render on the right.
right_candidates=()
if [ -n "${fh_pct:-}" ]; then
    fh_int=$(printf '%.0f' "$fh_pct" 2>/dev/null || echo 0)
    fh_bar=$(build_subtle_bar "$fh_int" 10)
    fh_bar_sm=$(build_subtle_bar "$fh_int" 6)
    fh_when=""
    if [ -n "${fh_reset:-}" ] && [ "${fh_reset}" != "0" ]; then
        fh_when=$(format_time "$fh_reset" time) || fh_when=""
    fi

    sd_int=""
    sd_bar=""
    sd_bar_sm=""
    sd_when=""
    if [ -n "${sd_pct:-}" ]; then
        sd_int=$(printf '%.0f' "$sd_pct" 2>/dev/null || echo 0)
        sd_bar=$(build_subtle_bar "$sd_int" 10)
        sd_bar_sm=$(build_subtle_bar "$sd_int" 6)
        if [ -n "${sd_reset:-}" ] && [ "${sd_reset}" != "0" ]; then
            sd_when=$(format_time "$sd_reset" datetime) || sd_when=""
            sd_when_short=$(format_time "$sd_reset" date) || sd_when_short=""
        fi
    fi

    if [ -n "$sd_int" ]; then
        # Tier A: full labels + both bars + both resets
        a="${DIM}current${RESET} ${fh_bar} ${DIM}${fh_int}%${RESET}"
        [ -n "$fh_when" ] && a="${a} ${DIM}⟳ ${fh_when}${RESET}"
        a="${a}   ${DIM}weekly${RESET} ${sd_bar} ${DIM}${sd_int}%${RESET}"
        [ -n "$sd_when" ] && a="${a} ${DIM}⟳ ${sd_when}${RESET}"
        right_candidates+=("$a")

        # Tier B: weekly reset shortened to date only
        b="${DIM}current${RESET} ${fh_bar} ${DIM}${fh_int}%${RESET}"
        [ -n "$fh_when" ] && b="${b} ${DIM}⟳ ${fh_when}${RESET}"
        b="${b}   ${DIM}weekly${RESET} ${sd_bar} ${DIM}${sd_int}%${RESET}"
        [ -n "${sd_when_short:-}" ] && b="${b} ${DIM}⟳ ${sd_when_short}${RESET}"
        right_candidates+=("$b")

        # Tier C: no labels, short bars, both resets (5h time, 7d date)
        c="${fh_bar_sm} ${DIM}${fh_int}%${RESET}"
        [ -n "$fh_when" ] && c="${c} ${DIM}⟳ ${fh_when}${RESET}"
        c="${c}  ${sd_bar_sm} ${DIM}${sd_int}%${RESET}"
        [ -n "${sd_when_short:-}" ] && c="${c} ${DIM}⟳ ${sd_when_short}${RESET}"
        right_candidates+=("$c")

        # Tier D: no bars, compact prefixes
        d="${DIM}5h${RESET} ${DIM}${fh_int}%${RESET}"
        [ -n "$fh_when" ] && d="${d} ${DIM}⟳ ${fh_when}${RESET}"
        d="${d}  ${DIM}7d${RESET} ${DIM}${sd_int}%${RESET}"
        [ -n "${sd_when_short:-}" ] && d="${d} ${DIM}⟳ ${sd_when_short}${RESET}"
        right_candidates+=("$d")

        # Tier E: percentages only
        right_candidates+=("${DIM}5h ${fh_int}%  7d ${sd_int}%${RESET}")
    fi

    # Tier F: 5h only, full label + bar + reset
    f="${DIM}current${RESET} ${fh_bar} ${DIM}${fh_int}%${RESET}"
    [ -n "$fh_when" ] && f="${f} ${DIM}⟳ ${fh_when}${RESET}"
    right_candidates+=("$f")

    # Tier G: 5h only, no bar
    g="${DIM}5h${RESET} ${DIM}${fh_int}%${RESET}"
    [ -n "$fh_when" ] && g="${g} ${DIM}⟳ ${fh_when}${RESET}"
    right_candidates+=("$g")

    # Tier H: 5h percent only
    right_candidates+=("${DIM}5h ${fh_int}%${RESET}")
fi

# Resolve terminal width (Claude Code doesn't pass it via stdin).
# Leave a safety margin — Claude Code reserves some right-edge columns
# (scroll indicator, overflow marker) so tput's raw width overshoots.
cols=0
if [ -r /dev/tty ]; then
    cols=$(tput cols 2>/dev/null </dev/tty || echo 0)
fi
if [ "$cols" = "0" ]; then
    cols="${COLUMNS:-0}"
fi
if [ "$cols" = "0" ]; then
    cols=$(tput cols 2>/dev/null || echo 0)
fi
safety=6
usable=$((cols - safety))

# Pick the longest rate-limit candidate that fits; otherwise drop it entirely.
right=""
if [ "${#right_candidates[@]}" -gt 0 ] && [ "$cols" -gt 0 ]; then
    left_len=$(visual_len "$line1")
    for cand in "${right_candidates[@]}"; do
        cand_len=$(visual_len "$cand")
        if [ $((left_len + cand_len + 2)) -le "$usable" ]; then
            right="$cand"
            break
        fi
    done
fi

if [ -n "$right" ]; then
    left_len=$(visual_len "$line1")
    right_len=$(visual_len "$right")
    gap=$((usable - left_len - right_len))
    [ "$gap" -lt 2 ] && gap=2
    pad=""
    i=0
    while [ "$i" -lt "$gap" ]; do
        pad="${pad} "
        i=$((i + 1))
    done
    printf "%b%s%b\n" "$line1" "$pad" "$right"
else
    printf "%b\n" "$line1"
fi
