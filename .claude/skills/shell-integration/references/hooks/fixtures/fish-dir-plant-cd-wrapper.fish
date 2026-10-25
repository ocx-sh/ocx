# PLANT: wraps cd; pushd, popd and `builtin cd` bypass it
function cd
    builtin cd $argv
    and echo FIRED:ours_dir
end
