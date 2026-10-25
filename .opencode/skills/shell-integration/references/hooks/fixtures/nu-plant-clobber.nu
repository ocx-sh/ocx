# PLANT: assigns the hook lists, dropping whatever the user's config.nu registered
$env.config.hooks.pre_prompt = [ {|| print "FIRED:ours_pp" } ]
$env.config.hooks.env_change.PWD = [ {|b, a| print "FIRED:ours_pwd" } ]
