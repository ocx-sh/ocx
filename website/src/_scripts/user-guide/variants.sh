ocx index list "python" --variants
ocx package install "python:debug-3.13.0"
ocx package exec "python:debug-3.13.0" -- python3 --version
