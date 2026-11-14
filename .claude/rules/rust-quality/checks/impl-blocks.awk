# impl-blocks.awk - list each file with three or more inherent impl blocks of one type. POSIX awk.
FNR == 1 { delete n; hit = 0 }
/^impl(<[^>]*>)?[ \t]+[A-Za-z0-9_]+(<[^>]*>)?[ \t]*\{/ {
  name = $0; sub(/^impl(<[^>]*>)?[ \t]+/, "", name); sub(/[^A-Za-z0-9_].*/, "", name)
  if (++n[name] == 3 && !hit) { print FILENAME; hit = 1 }
}
