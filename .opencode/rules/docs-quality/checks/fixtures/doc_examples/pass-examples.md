<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

# Publish a package

Run the publish command.

```bash-run
# doc: publish-a-package
ocx push ./dist
```

<!-- doc-norun: this command needs a registry token the reader does not have -->

```bash-norun
ocx login --token "$TOKEN"
```

<!-- /doc-norun -->

The output lists one digest per platform.

```text
sha256:abc123
```
