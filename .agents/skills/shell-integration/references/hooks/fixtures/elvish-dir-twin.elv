# TWIN: a marker as the closure rest-argument name, probed in the live list before appending
if (not (has-value [(all $after-chdir | each {|c| if (==s (kind-of $c) fn) { all $c[arg-names] } })] '__x_hook_marker')) {
  set after-chdir = [ $@after-chdir {|@__x_hook_marker| echo FIRED:ours_dir } ]
}
