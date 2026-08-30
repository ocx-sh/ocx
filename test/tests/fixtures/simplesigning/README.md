# simplesigning negative fixtures (test only)

Committed bytes for the cosign *simplesigning* read path
(`crates/ocx_lib/src/oci/verify/simplesigning_read.rs`). Spec D7: simplesigning
read fixtures are committed bytes, never generated at test time — a fixture a
test mints is a fixture nobody can review, and a reader that normalises bytes on
the way in cannot be caught by one.

The **positive** fixtures live next door in
[`../golden/`](../golden/) and are cosign v3.1.1's own output
(`simplesigning_key_*`, `simplesigning_keyless_*`). Everything here is an
OCX-authored *negative*: cosign will not produce a splice or a rogue chain, so
these cannot come from a capture.

| File | Derived from | The one thing it changes | Read by |
|---|---|---|---|
| `foreign_subject_payload.json` | `golden/simplesigning_keyless_payload.json` | `critical.image.docker-manifest-digest` names another manifest — the cross-subject splice (S-006) | `simplesigning_read.rs::the_claim_reader_refuses_a_payload_naming_another_manifest` |
| `foreign_claim_type_payload.json` | same | `critical.type` is not `cosign container image signature` | `simplesigning_read.rs::the_claim_reader_refuses_another_claim_type` |
| `untrusted_ca_manifest.json` | `golden/simplesigning_keyless_manifest.json` | the certificate annotation is a leaf from an unrelated CA (S-014) | `simplesigning_read.rs::a_sidecar_certificate_outside_the_fulcio_root_is_refused`, plus a chain-gate test in `attestation_sidecar.rs` |
| `tampered_signature_manifest.json` | same | one byte flipped inside the DER ECDSA signature | `simplesigning_read.rs::a_tampered_sidecar_signature_is_refused`, and (the only fixtures here that do) pushed into a real registry and refused through the shipped binary by `test_verify.py`'s cosign-interop cell |
| `tampered_key_signature_manifest.json` | `golden/simplesigning_key_manifest.json` | the same flip, key mode — no certificate to fall back on | nothing in Rust — only `test_verify.py`'s cosign-interop cell exercises it |
| `publisher_formatted_payload.json` | `golden/simplesigning_key_payload.json` | the same claim, indented with a trailing newline instead of compact — a genuine signature over that formatting | `simplesigning_read.rs::the_signature_covers_the_served_bytes_not_a_re_serialized_claim` |
| `publisher_formatted_manifest.json` | `golden/simplesigning_key_manifest.json` | carries the payload above as its sidecar layer | same test |

Every fixture above is read directly by a `simplesigning_read.rs` unit test
except `tampered_key_signature_manifest.json`, which has no Rust consumer —
key-mode tampering is proven only through the Python cell. Only the two
`tampered_*` fixtures additionally round-trip through a real registry:
`test/tests/test_verify.py`'s cosign-interop cells push them and assert the
shipped binary refuses them. Both flips land at the same offset (byte 8 of
the DER signature), so the key-mode and keyless negatives differ only in the
material they carry — which is the axis the two cells are about.

## Why `untrusted_ca_manifest.json` keeps the genuine identity

Its leaf carries the **same** SubjectAltName (`ocx-test@example.com`) and the
same Fulcio OIDC-issuer extension (`1.3.6.1.4.1.57264.1.8`,
`http://dex:5556/dex`) as the genuine certificate, and the same
`keyUsage`/`extendedKeyUsage` a Fulcio leaf carries. That is deliberate: if it
were identity-unacceptable too, a test asserting `cert_chain_invalid` could not
tell the chain check from the identity check, and deleting the chain check would
leave the test green.

It is signed by a throwaway P-256 CA that exists nowhere — not in
`test/sigstore/trusted_root.json`, not on disk. The private keys were discarded
after minting; nothing here can sign anything.

Regenerate with `openssl` (the CA is disposable, so any fresh one works):

```sh
openssl ecparam -name prime256v1 -genkey -noout -out ca.key
openssl req -x509 -new -key ca.key -sha256 -days 3650 -out ca.crt \
  -subj '/O=rogue test/CN=rogue test CA' \
  -addext 'basicConstraints=critical,CA:TRUE' \
  -addext 'keyUsage=critical,keyCertSign,cRLSign'
openssl ecparam -name prime256v1 -genkey -noout -out leaf.key
openssl req -new -key leaf.key -subj '/' -out leaf.csr
cat > leaf.cnf <<'EOF'
[v3_leaf]
basicConstraints = critical,CA:FALSE
keyUsage = critical,digitalSignature
extendedKeyUsage = codeSigning
subjectKeyIdentifier = hash
authorityKeyIdentifier = keyid
subjectAltName = critical,email:ocx-test@example.com
1.3.6.1.4.1.57264.1.8 = ASN1:UTF8String:http://dex:5556/dex
EOF
openssl x509 -req -in leaf.csr -CA ca.crt -CAkey ca.key -set_serial 1 \
  -days 3650 -sha256 -extfile leaf.cnf -extensions v3_leaf -out leaf.crt
```

Then paste `leaf.crt` into the `dev.sigstore.cosign/certificate` annotation of a
copy of `golden/simplesigning_keyless_manifest.json`.
