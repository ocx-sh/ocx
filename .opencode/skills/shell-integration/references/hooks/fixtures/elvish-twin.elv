# TWIN: the marker is the closure rest-argument name, probed in the live hook list
if (not (has-value [(all $edit:before-readline | each {|c| if (==s (kind-of $c) fn) { all $c[arg-names] } })] '__x_hook_marker')) {
  set edit:before-readline = [ $@edit:before-readline {|@__x_hook_marker| echo FIRED:ours } ]
}
