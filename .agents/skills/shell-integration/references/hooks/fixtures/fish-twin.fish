# TWIN: an event handler; redefinition replaces it, the user's fish_prompt and handlers are untouched
function __x_hook --on-event fish_prompt
    echo FIRED:ours
end
