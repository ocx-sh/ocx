ocx package install "cmake:4.2.0"
ocx package select "cmake:4.2.0"
ocx package exec "cmake:4.2.0" -- cmake --version
ocx package env "cmake:4.2.0"
