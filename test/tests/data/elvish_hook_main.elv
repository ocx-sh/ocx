try { eval 'if (not (has-value [(all $edit:before-readline | each {|__ocx_candidate| if (==s (kind-of $__ocx_candidate) fn) { all $__ocx_candidate[arg-names] } })] ''__ocx-prompt-hook'')) {
set edit:before-readline = [$@edit:before-readline {|@__ocx-prompt-hook|
if (or (==s $E:__OCX_ENV_STATE '''') (!=s $E:__OCX_ENV_PWD (to-string $pid)'' ''$pwd'' ''$E:DIRENV_DIR'' ''$E:MISE_SHELL'' ''$E:__MISE_ORIG_PATH'' ''$E:OCX_CONSENT_PATHS'' ''$E:OCX_CONSENT_NAMESPACES)) {
try { eval (''@OCX_BINARY@'' --offline self activate --reconcile --shell=elvish 2>/dev/null | slurp) } catch e { }
}
}]
}' } catch e { }
