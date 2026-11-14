# multiline.awk - grep -n for an ERE that may span lines. POSIX awk.
# Env PAT: the ERE; the two characters \n inside it mean a newline.
#     NOT: optional ERE; a match whose text also matches it is skipped.
#     L:   set to 1 to print each matching file once (grep -l).
# Prints FILE:LINE:TEXT, LINE being where the match starts. TEXT is the first line of the match,
# taken from the match offset, so a match that starts mid-line prints the tail of that line.
# Anchors: each file is ONE record, so ^ and $ match only at the start and end of the file (and ^
# again at the line after each reported match). Anchor on a line with \n in PAT, never ^ or $.
BEGIN {
  RS = "\001"
  pat = ENVIRON["PAT"]; gsub(/\\n/, "\n", pat)
  skip = ENVIRON["NOT"]; gsub(/\\n/, "\n", skip)
}
{
  rest = $0; line = 1
  while (match(rest, pat)) {
    s = RSTART; m = substr(rest, s, RLENGTH)
    pre = substr(rest, 1, s - 1); line += gsub(/\n/, "&", pre)
    if (skip == "" || m !~ skip) {
      if (ENVIRON["L"] == 1) { print FILENAME; break }
      first = m; sub(/\n.*/, "", first); print FILENAME ":" line ":" first
    }
    nl = index(substr(rest, s), "\n")
    if (!nl) break
    rest = substr(rest, s + nl); line++
  }
}
