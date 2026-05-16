ocx init
ocx add "cmake:4.2.0"
ocx run -- cmake --version
ocx package exec "cmake:4.2.0" -- cmake --version
