<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

# Mirror a registry

Copy every artifact from one registry to another and verify the digests.

## Copy the artifacts

Run the mirror command against both registries.

```bash
grim mirror --from src.example --to dst.example
```

## Verify the copy

Compare the source and destination digests.

```bash
grim verify dst.example
```
