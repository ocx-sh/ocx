# TWIN: add-zsh-hook dedupes by function name and leaves chpwd() and the arrays alone
__x_dir() { print FIRED:ours_dir }
autoload -Uz add-zsh-hook
add-zsh-hook chpwd __x_dir
