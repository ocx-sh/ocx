# TWIN: normalise to a list, drop our previous closure (found by a marker comment in its source), append the new one
def __x_hooks_without_ours [cur: any] {
    let l = if (($cur | describe) starts-with 'list') { $cur } else if $cur == null { [] } else { [$cur] }
    $l | where {|h| not (($h | describe) == 'closure' and ((view source $h) | str contains '__x_hook_marker')) }
}
$env.config.hooks.pre_prompt = (__x_hooks_without_ours ($env.config.hooks.pre_prompt? | default []) | append {|| # __x_hook_marker
    print "FIRED:ours_pp" })
$env.config.hooks.env_change.PWD = (__x_hooks_without_ours ($env.config.hooks.env_change.PWD? | default []) | append {|b, a| # __x_hook_marker
    print "FIRED:ours_pwd" })
