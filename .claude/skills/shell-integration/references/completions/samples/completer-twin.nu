# Twin: chains to the previously installed completer for every command that is not ours.
$env.config.completions.external.enable = true
let __prev = ($env.config.completions.external.completer)
$env.config.completions.external.completer = {|spans|
    if $spans.0 == 'demo' { [sub help] } else if $__prev != null { do $__prev $spans }
}
