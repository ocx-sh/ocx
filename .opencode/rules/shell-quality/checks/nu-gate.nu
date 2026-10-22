#!/usr/bin/env nu
# nu-range: 0.100.0..0.116.1
# nu-gate.nu: parse gate for emitted or hand-written nushell. Exit 1 on ANY of:
#   (1) an --ide-check diagnostic (parse/type errors; that flag always exits 0, so its JSON lines are the signal)
#   (2) a parse-time Warning or Error from 'if false { source FILE }' (deprecations print only at parse of the
#       sourced file, never under --ide-check or nu-check; the dead branch means FILE is parsed, not run)
#   (3) a bareword command that is neither built-in, custom nor ^-prefixed and not on PATH (--ide-ast
#       shape_external): the parser turns a removed or unknown command into an external call
#   (4) a call to a command the running nu itself files under category "removed" (renamed and removed names)
# --allow takes a comma-separated STRING: a list<string> flag given on the command line parses as nuon on nu <= 0.110
# and is a type mismatch on nu >= 0.114.
# Written in the safe intersection: it runs on nu 0.100.0 .. 0.116.1.
# --strict: ANY bareword external is red unless listed in --allow (catches the removed size command, which a binutils binary answers).
# usage: nu nu-gate.nu [--strict] [--allow ocx,git] file.nu ...
def main [--allow: string = "", --strict, ...files: path] {
  let exe = $nu.current-exe
  let allowed = ($allow | split row "," | where {|a| $a != "" })
  let removed = (scope commands | where category == "removed" | get name)
  mut bad = 0
  for f in $files {
    let abs = ($f | path expand)
    let chk = (^$exe --ide-check 100 $abs | complete)
    for d in ($chk.stdout | lines | where {|l| $l | str starts-with '{"type":"diagnostic"'}) {
      print -e $"($f): diagnostic: ($d)"
      $bad = $bad + 1
    }
    let wrap = (mktemp --tmpdir nu-gate-XXXXXX.nu)
    $"if false { source '($abs)' }\n" | save --force $wrap
    let dry = (^$exe --no-config-file --error-style plain $wrap | complete)
    rm --force $wrap
    for l in ($dry.stderr | lines | where {|l| ($l | str starts-with 'Warning:') or ($l | str starts-with 'Error:')}) {
      print -e $"($f): ($l)"
      $bad = $bad + 1
    }
    let ast = (^$exe --ide-ast $abs | complete | get stdout | from json)
    let raw = (open --raw $abs | into binary)
    for n in ($ast | where shape == "shape_external") {
      let caret = if $n.span.start > 0 { ($raw | bytes at ($n.span.start - 1)..($n.span.start - 1) | decode utf-8) == "^" } else { false }
      if (not $caret) and (not ($n.content in $allowed)) and ($strict or (which $n.content | is-empty)) {
        print -e $"($f): unknown command treated as external: ($n.content)"
        $bad = $bad + 1
      }
    }
    for n in ($ast | where shape == "shape_internalcall") {
      if $n.content in $removed {
        print -e $"($f): removed command: ($n.content)"
        $bad = $bad + 1
      }
    }
  }
  if $bad > 0 { exit 1 }
}
