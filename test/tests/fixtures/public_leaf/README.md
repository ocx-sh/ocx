# Recorded public Fulcio leaf

`github_actions_leaf.pem` is a real certificate Fulcio issued to a GitHub Actions
workflow, and `github_actions_leaf.json` records where it came from (its public
Rekor entry) and the extension values it carries.

It is public-good Sigstore material, so it lives outside `golden/`, which holds only
what `golden/generate.py` reproduces from the local stack. It is read by the claim
parsing tests in `crates/ocx_sign/src/verify/identity.rs` and nothing else: no test
verifies its signature, its chain or its log entry against a public trust root.
