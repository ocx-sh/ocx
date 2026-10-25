#!/bin/sh
# nushell-hang.sh: does an external in tail position of `try` hang on this nu build (SH-NU-11)?
# usage: NU=/path/to/nu sh nushell-hang.sh      (run once per pinned nu: the floor and the ceiling)
# exit 0: the plant finishes, the hazard is not live on this build
# exit 1: the plant hangs past 10 seconds, so a tail-position external in `try` is a defect on this build
# exit 69: nu or timeout missing    exit 74: mktemp failed, or a probe failed for another reason
NU=${NU:-nu}
command -v "$NU" >/dev/null 2>&1 || {
    echo "nushell-hang: $NU not found" >&2
    exit 69
}
command -v timeout >/dev/null 2>&1 || {
    echo "nushell-hang: timeout not found" >&2
    exit 69
}
d=$(mktemp -d) || exit 74
trap 'rm -rf "$d"' EXIT
body='head -c 300000 /dev/zero | tr "\0" a'
printf '%s\n' "let r = (try { ^sh -c '$body' } catch { \"caught\" })" "print (\$r | str length)" >"$d/plant.nu"
printf '%s\n' "let r = (try { ^sh -c '$body' | complete } catch { {stdout: \"\", exit_code: 1} })" "print (\$r.stdout | str length)" >"$d/twin.nu"
if ! timeout 10 "$NU" --no-config-file "$d/twin.nu" >"$d/twin.out" 2>&1; then
    echo "nushell-hang: the complete-form twin failed on $NU" >&2
    exit 74
fi
timeout 10 "$NU" --no-config-file "$d/plant.nu" >"$d/plant.out" 2>&1
case $? in
    0) exit 0 ;;
    124 | 137 | 143)
        echo "nushell-hang: $NU hangs on an external in tail position of try, add | complete inside"
        exit 1
        ;;
    *)
        echo "nushell-hang: the plant failed for another reason on $NU" >&2
        exit 74
        ;;
esac
