# PLANT: appends with no dedupe, sourcing twice registers two copies
$env.config.hooks.pre_prompt = ($env.config.hooks.pre_prompt? | default [] | append {|| print "FIRED:ours_pp" })
$env.config.hooks.env_change.PWD = ($env.config.hooks.env_change.PWD? | default [] | append {|b, a| print "FIRED:ours_pwd" })
