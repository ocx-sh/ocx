# first-param.awk - list each file where three or more multi-parameter fns take the same first
# parameter type. POSIX awk.
BEGIN { RS = "\001" }
{
  rest = $0; delete n
  while (match(rest, /fn [A-Za-z0-9_]+\([[:space:]]*[A-Za-z0-9_]+: &?(mut )?[A-Z][A-Za-z0-9_]+[^,)]*,/)) {
    sig = substr(rest, RSTART, RLENGTH); rest = substr(rest, RSTART + RLENGTH)
    sub(/^fn [A-Za-z0-9_]+\([[:space:]]*[A-Za-z0-9_]+: /, "", sig)
    match(sig, /^&?(mut )?[A-Z][A-Za-z0-9_]+/); t = substr(sig, 1, RLENGTH)
    if (++n[t] == 3) { print FILENAME; break }
  }
}
