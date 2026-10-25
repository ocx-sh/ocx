# Plant: assigns the single external-completer slot, replacing whatever the user's config.nu installed.
$env.config.completions.external.enable = true
$env.config.completions.external.completer = {|spans| if $spans.0 == 'demo' { [sub help] } }
