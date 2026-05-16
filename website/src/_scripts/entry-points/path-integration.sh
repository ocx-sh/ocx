ocx package install --select "uv:0.10.0"

# Verify the current symlink was created (--select happened during install).
ocx package which --current "uv:0.10.0" >/dev/null

# Print the resolved env for the installed package (the eval-safe form).
# This is the per-package equivalent of shell profile activation.
ocx package env --shell=bash "uv:0.10.0" >/dev/null
