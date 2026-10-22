# PLANT: unguarded array append; sourcing twice registers two copies
__x_hook() { print FIRED:ours }
precmd_functions+=(__x_hook)
