# TWIN: add-zsh-hook dedupes and leaves precmd() and the array alone (zsh 5.0.8-5.9)
__x_hook() { print FIRED:ours }
autoload -Uz add-zsh-hook
add-zsh-hook precmd __x_hook
