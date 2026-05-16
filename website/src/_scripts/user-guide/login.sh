echo "test-token" | ocx login -u ci --password-stdin --allow-insecure-store "$DEMO_REGISTRY"
ocx logout "$DEMO_REGISTRY"
