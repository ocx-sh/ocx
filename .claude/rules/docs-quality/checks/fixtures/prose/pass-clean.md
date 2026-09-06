<!-- doc_type: how-to -->
<!-- doc_tier: everyday -->

# Publish a package

Run one command to publish a package to a registry you already trust.

## Before you start

You need a registry login and a built package on disk.

```bash
grim publish ./dist/example.tar
```

The command prints the digest it pushed.

## Check the result

Read the digest back from the registry and compare it.

```bash
grim describe example
```

A digest that differs means the push raced another writer.

## See also

Read the registry reference page for the full flag list.
