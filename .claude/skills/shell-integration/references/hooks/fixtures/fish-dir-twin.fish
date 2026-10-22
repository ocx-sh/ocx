# TWIN: --on-variable PWD with a unique name; redefinition replaces
function __x_dir --on-variable PWD
    echo FIRED:ours_dir
end
