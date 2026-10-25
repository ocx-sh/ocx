# PLANT: unguarded append; an rc.elv re-sourced or `exec elvish` registers a second copy
set edit:before-readline = [ $@edit:before-readline { echo FIRED:ours } ]
