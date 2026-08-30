# The golden-fixture signing key (test only)

`cosign.key` and `cosign.pub` are the key pair the **key-mode** golden fixtures
in the parent directory were signed with. `cosign.key` is encrypted; the
password is `ocxtest`, and it is written down in `generate.py` as
`KEY_PASSWORD` because it is not protecting anything.

## Deliberately committed, and not a secret

Same posture as [`test/sigstore/keys`](../../../../sigstore/README.md), for the
same reasons:

- It signs nothing outside this repository's fixtures. Every artifact it
  produced lives in this directory.
- It is worthless to an attacker. No ocx verification path trusts it: a
  verifier reaches it only when a caller names it, and nothing in the product
  does.
- It is committed rather than generated per run **so the fixtures can be
  committed too**. `key_bundle.json` carries a `publicKey` hint that names this
  key, and `simplesigning_key_manifest.json` carries a signature only this key
  can produce. A key that rotated on every capture would be a key no test could
  pin — the same argument that keeps `trusted_root.json` in the tree.

`generate.py` therefore never regenerates the pair when it is present. Rotating
it means deleting both files, re-running `--regenerate`, and re-reading every
test that pinned the old public key.

## Regenerating

```sh
cd test
rm tests/fixtures/golden/keys/cosign.key tests/fixtures/golden/keys/cosign.pub
uv run python3 tests/fixtures/golden/generate.py --regenerate
```

The pair is minted by `cosign generate-key-pair` inside the same pinned cosign
container the fixtures come from, so its format is whatever that version emits
(currently an encrypted ECDSA P-256 key in cosign's own PEM envelope) rather
than whatever the host's OpenSSL happens to default to.
