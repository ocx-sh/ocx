ocx package install --select "uv:0.10.0"

# Print the eval-safe env for the selected package.
# In a shell profile, this lets launchers declared in the package's metadata
# appear on $PATH.  The global toolchain form (eval "$(ocx --global env --shell=bash)")
# is used when the package is managed via ocx.toml.
ocx package env --shell=bash "uv:0.10.0"
