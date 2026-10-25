# go-volatile-xdefs.awk - print every {KEY} whose KEY is not STABLE_-prefixed (a grep -P lookahead twin). POSIX awk.
# Usage: git ls-files -z ... | xargs -0 -r awk -f go-volatile-xdefs.awk --   Prints FILE:LINE:{KEY}.
{
  s = $0
  while (match(s, /\{[A-Z][A-Z0-9_]*\}/)) {
    tok = substr(s, RSTART, RLENGTH)
    if (tok !~ /^\{STABLE_/) print FILENAME ":" FNR ":" tok
    s = substr(s, RSTART + RLENGTH)
  }
}
