# PLANT: unguarded append; sourced twice it registers twice
set after-chdir = [ $@after-chdir {|d| echo FIRED:ours_dir } ]
