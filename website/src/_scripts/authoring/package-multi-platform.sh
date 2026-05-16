ocx package create build -i mytool:1.0.0 -p linux/amd64 -m metadata.json -o .
ocx package create build -i mytool:1.0.0 -p linux/arm64 -m metadata.json -o .
ocx package push -n -c -p linux/amd64 -i mytool:1.0.0 mytool-1.0.0-linux-amd64.tar.xz
ocx package push -c -p linux/arm64 -i mytool:1.0.0 mytool-1.0.0-linux-arm64.tar.xz
ocx index update mytool
ocx index list mytool --platforms
ocx package install mytool:1.0.0
ocx package exec mytool:1.0.0 -- mytool
