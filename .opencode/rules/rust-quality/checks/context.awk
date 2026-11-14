# context.awk - grep -n -A/-B for an ERE. POSIX awk.
# Env PAT: the ERE.  A: lines after each hit (default 0).  B: lines before (default 0).
# Prints FILE:LINE:TEXT for a hit, FILE-LINE-TEXT for context, -- between groups.
BEGIN { pat = ENVIRON["PAT"]; a = ENVIRON["A"] + 0; b = ENVIRON["B"] + 0 }
FNR == 1 { done = 0; left = 0; delete hist }
{
  hist[FNR] = $0; delete hist[FNR - b - 1]
  if ($0 ~ pat) {
    from = FNR - b; if (from <= done) from = done + 1
    if (done && from > done + 1) print "--"
    for (i = from; i < FNR; i++) print FILENAME "-" i "-" hist[i]
    print FILENAME ":" FNR ":" $0
    done = FNR; left = a
  } else if (left > 0) {
    print FILENAME "-" FNR "-" $0; done = FNR; left--
  }
}
