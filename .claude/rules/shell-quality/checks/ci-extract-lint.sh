#!/bin/sh
# ci-extract-lint.sh MODE: run ShellCheck over shell bodies that live inside YAML.
# Modes: gitlab and gitlab-bash (SH-CI-28, SH-CI-29: GitLab script blocks as sh or as bash),
#        taskfile (SH-CI-19: Taskfile cmds as bash).
# Needs python3 with PyYAML in its own site (python3 -I) next to ci-gl-extract.py and ci-tf-extract.py.
# A failing extractor is a failure, never a silent pass.
# Exit: 0 clean, 1 finding or extractor failure, 64 bad mode, 69 tool missing, 78 not a git work tree.
set -u
mode=${1:-}
case $mode in
    gitlab | gitlab-bash | taskfile) ;;
    *)
        echo "usage: ci-extract-lint.sh gitlab|gitlab-bash|taskfile" >&2
        exit 64
        ;;
esac
here=$(dirname "$0")
for t in git xargs python3 shellcheck mktemp; do
    command -v "$t" >/dev/null 2>&1 || {
        echo "ci-extract-lint: $t missing" >&2
        exit 69
    }
done
git rev-parse --is-inside-work-tree >/dev/null 2>&1 || {
    echo "ci-extract-lint: not a git work tree" >&2
    exit 78
}
python3 -I -c 'import yaml' >/dev/null 2>&1 || {
    echo "ci-extract-lint: PyYAML missing for python3 -I" >&2
    exit 69
}
tmp=$(mktemp -d) || exit 69
trap 'rm -rf "$tmp"' EXIT
case $mode in
    taskfile) git ls-files -z --cached --others --exclude-standard -- ':(glob)**/Taskfile*.yml' ':(glob)**/taskfile.yml' ':(glob)**/*.taskfile.yml' ':(exclude,glob)**/testdata/**' ;;
    *) git ls-files -z --cached --others --exclude-standard -- ':(glob)**/.gitlab-ci.yml' ':(glob)**/*.gitlab-ci.yml' ':(glob)**/.gitlab-ci/**/*.yml' ':(glob)**/.gitlab/ci/**/*.yml' ;;
esac | tr '\0' '\n' >"$tmp/list"
status=0
n=0
# lint DIR FILE SHELLCHECK_ARGS...: run ShellCheck and name the YAML file the findings came from
lint() {
    dir=$1 src=$2
    shift 2
    res=$(shellcheck "$@") || status=1
    [ -n "$res" ] || return 0
    printf '%s\n' "$src"
    printf '%s\n' "$res" | sed "s|$dir/||"
}
while IFS= read -r f; do
    n=$((n + 1))
    d="$tmp/$n"
    mkdir "$d"
    case $mode in
        taskfile)
            python3 -I "$here/ci-tf-extract.py" "$f" >"$d/body.sh" || {
                echo "$f: extractor failed"
                status=1
                continue
            }
            lint "$d" "$f" -s bash -e SC1091,SC2050,SC2194,SC2154,SC2157,SC2043,SC2234 -f gcc -- "$d/body.sh"
            ;;
        *)
            flag=--sh
            dialect="sh"
            [ "$mode" = gitlab-bash ] && flag=--bash && dialect=bash
            python3 -I "$here/ci-gl-extract.py" "$flag" "$f" "$d" || {
                echo "$f: extractor failed"
                status=1
                continue
            }
            for s in "$d"/*.sh; do
                [ -e "$s" ] || continue
                lint "$d" "$f" -s "$dialect" -f gcc -- "$s"
            done
            ;;
    esac
done <"$tmp/list"
exit "$status"
