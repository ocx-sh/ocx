# test-windows-nu-activation.nu - nushell activation gate.
#
# Usage: nu test/manual/test-windows-nu-activation.nu <path-to-ocx-binary>
#
# Counterpart of test-windows-git-bash-activation.sh for a native nushell. It seeds a
# throwaway OCX_HOME, lets `ocx self setup` write env.nu, sources that shim the way the
# vendor-autoload file does (a child `nu --no-config-file`, since `source` needs a parse-time
# path) and checks the bin directory is on PATH, `ocx version` runs, and the per-prompt hook
# closure runs clean. The child gets no HOME on Windows, as a native process has none.
# It runs twice: once with OCX_HOME seeded, and once with it unset and the store under a
# throwaway USERPROFILE (HOME on unix), which exercises the shim's own default. Both runs use
# `--no-modify-path --no-profile`, so no registry PATH or profile file is touched and neither
# needs a CI gate. HOME is removed on Windows, as a native process has none.

# Normalise a path for comparison: the shim joins with `/` where the caller holds `\`.
def norm []: string -> string {
    $in | str replace --all '\' '/'
}

# Run a closure with the given variables and no other ocx ones, whatever the runner exported.
def hermetic [vars: record, body: closure] {
    let keep = ($vars | columns)
    let stale = ($env | columns | where {|c| (($c | str starts-with 'OCX_') or ($c | str starts-with '__OCX_')) and not ($c in $keep) })
    with-env $vars {
        for n in $stale { hide-env -i $n }
        do $body
    }
}

# One seeded-store run: setup, then a child nu that sources env.nu and probes. Returns the
# printed check lines' tally as {passes, failures}.
def run-case [label: string, vars: record, store: path, work: path, src: path, bin_name: string, is_windows: bool] {
    let bin_dir = ($store | path join 'symlinks' 'ocx.sh' 'ocx' 'cli' 'current' 'content' 'bin')
    mkdir $bin_dir
    let ocx_bin = ($bin_dir | path join $bin_name)
    cp $src $ocx_bin

    print $"\n[($label)]"
    print $"  ocx  : ($ocx_bin)"
    print $"  store: ($store)"

    let setup = (hermetic $vars { if $is_windows { hide-env -i HOME }; ^$ocx_bin --offline self setup --no-modify-path --no-profile | complete })
    let env_nu = ($store | path join 'env.nu')
    if $setup.exit_code != 0 or not ($env_nu | path exists) {
        print $setup.stdout $setup.stderr
        print -e $"ocx self setup failed or wrote no env.nu: ($env_nu)"
        rm -rf $work
        exit 2
    }

    # The child starts in the scratch directory: the repository's own ocx.toml would make the hook report on it.
    # It runs in the shape the autoload gives a real session: shim sourced, then probes.
    let probe = $"
source '($env_nu)'
let segs = \($env.PATH | \(if \($in | describe\) == 'string' { split row \(char esep\) } else { $in }\)\)
print $'PATH=\($segs | str join \(char esep\)\)'
let version = \(try { ^ocx version | complete } catch { {exit_code: 127, stdout: '', stderr: 'ocx not found on PATH'} }\)
print $'WHICH=\(which ocx | get path | first 1 | str join)'
print $'VERSION_RC=\($version.exit_code\)'
print $'VERSION_ERR=\($version.stderr | str trim\)'
let hooks = \($env.config.hooks.pre_prompt? | default []\)
print $'HOOKS=\($hooks | length\)'
let hook = \(try { for h in $hooks { do $h }; 'ok' } catch {|e| $'error: \($e.msg\)' }\)
print $'HOOK=\($hook\)'
"
    let ran = (hermetic $vars { if $is_windows { hide-env -i HOME }; cd $work; ^$nu.current-exe --no-config-file -c $probe | complete })

    mut failures = 0
    mut passes = 0
    let out = ($ran.stdout | lines)
    let field = {|name| $out | where {|l| $l | str starts-with $"($name)=" } | first 1 | each {|l| $l | str replace $"($name)=" '' } | get 0? | default '' }

    # stderr is not asserted: the shim lets the reconcile report its PATH diff there.
    if $ran.exit_code == 0 {
        print '[PASS] 1 shim sourced and probed: exit 0'
        $passes += 1
    } else {
        print $"[FAIL] 1 shim sourcing: exit ($ran.exit_code); stderr: ($ran.stderr)"
        $failures += 1
    }

    let path_value = (do $field 'PATH')
    let want = ($bin_dir | norm)
    if ($path_value | split row (char esep) | any {|p| ($p | norm) == $want }) {
        print $"[PASS] 2 seeded bin dir on PATH: ($bin_dir)"
        $passes += 1
    } else {
        print $"[FAIL] 2 seeded bin dir missing from PATH: ($bin_dir)\n       PATH=($path_value)"
        $failures += 1
    }

    # Resolving to the seeded copy, not an ocx the runner already had, is what ties this to the shim.
    let resolved = (do $field 'WHICH' | norm)
    if (do $field 'VERSION_RC') == '0' and $resolved == ($ocx_bin | norm) {
        print "[PASS] 3 'ocx version' exits 0 through the seeded copy"
        $passes += 1
    } else {
        print $"[FAIL] 3 'ocx version' via ($resolved) expected ($ocx_bin | norm): rc=(do $field 'VERSION_RC') (do $field 'VERSION_ERR')"
        $failures += 1
    }

    let hooks = (do $field 'HOOKS')
    let hook = (do $field 'HOOK')
    if $hooks != '' and ($hooks | into int) >= 1 and $hook == 'ok' {
        print '[PASS] 4 pre_prompt hook registered and runs clean'
        $passes += 1
    } else {
        print $"[FAIL] 4 pre_prompt hook: registered=($hooks) result=($hook)"
        $failures += 1
    }
    {passes: $passes, failures: $failures}
}

def main [ocx_binary: path] {
    let is_windows = ($nu.os-info.name == 'windows')
    let bin_name = (if $is_windows { 'ocx.exe' } else { 'ocx' })
    let src = ($ocx_binary | path expand)
    if not ($src | path exists) {
        print -e $"ocx binary not found: ($src)"
        exit 2
    }

    let base = ($env.RUNNER_TEMP? | default ($env.TMPDIR? | default (if $is_windows { $env.TEMP } else { '/tmp' })))
    let work = ($base | path join $"ocx-nu-(random uuid)")
    mkdir $work
    print $"Running nushell activation gate \(os: ($nu.os-info.name)\)"

    let explicit = (run-case 'OCX_HOME set' {OCX_HOME: ($work | path join 'home')} ($work | path join 'home') $work $src $bin_name $is_windows)
    # The default store: no OCX_HOME, the profile directory (HOME on unix) holds `.ocx`.
    let profile = ($work | path join 'profile')
    mkdir $profile
    let default_case = (run-case 'OCX_HOME unset' {USERPROFILE: $profile, HOME: $profile} ($profile | path join '.ocx') $work $src $bin_name $is_windows)

    let passes = ($explicit.passes + $default_case.passes)
    let failures = ($explicit.failures + $default_case.failures)
    rm -rf $work
    print $"\n($passes) passed, ($failures) failed"
    if $failures > 0 {
        print $"($failures) ACTIVATION CHECK\(S\) FAILED \(nushell\)"
        exit 1
    }
    print 'ALL ACTIVATION CHECKS PASSED (nushell)'
}
