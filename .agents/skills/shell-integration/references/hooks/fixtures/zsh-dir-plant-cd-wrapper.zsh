# PLANT: wraps cd; pushd, popd and `builtin cd` bypass it
cd() { builtin cd "$@" && print FIRED:ours_dir }
