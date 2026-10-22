# TLS verification fixtures

These public-only certificates test the normal ring/WebPKI verification path
without network access, host trust-store assumptions, or additional Rust
dependencies. They contain no production identity, credential, or private key.

`root.der` is a synthetic P-256 root. `valid.der` is its server certificate for
the reserved name `tls-fixture.invalid`. `self-signed.der` uses the same synthetic
leaf key but is not signed by the root. All certificates have fixed validity
from 2026-01-01 through 2030-01-01. Tests supply 2026-01-02 00:00:00 UTC explicitly;
the expiry test supplies 2040-01-01 00:00:00 UTC. Neither depends on the wall clock.

The eight `EM*.der` leaves are also signed by the synthetic root, with the same
hostname and validity as the accepted leaf, but carry the public SPKIs distrusted
by rustls-webpki 0.103.16 for RUSTSEC-2026-0382 / CVE-2026-111943. Their source is
[rustls/webpki at commit 8ecdcc76f56b68fda67a6adf7976f5eed5006a4c](https://github.com/rustls/webpki/tree/8ecdcc76f56b68fda67a6adf7976f5eed5006a4c/src/data/distrusted).
`generate.py` pins each upstream SPKI SHA-256 before use. Upstream stores the
contents of SubjectPublicKeyInfo; the generator adds its outer DER SEQUENCE for
OpenSSL. `openssl x509 -new -force_pubkey` can issue these certificates without
the private keys corresponding to the distrusted public keys.

The regression requires the precise `CertDistrusted` error, exposed by rustls
as `CertificateError::Other`, rather than accepting any certificate error. With
rustls 0.23.46 and webpki 0.103.15 the first distrusted leaf is accepted and this
test fails; changing only webpki to 0.103.16 makes all eight leaves be rejected.
The independent positive-chain test ensures the synthetic root is trusted.

Regenerate deliberately with Python 3 and OpenSSL 3.4 or newer:

```sh
python tests/fixtures/tls/generate.py
cargo test --locked tls::
```

Generation downloads only the pinned public SPKIs and creates fresh synthetic
keys in a temporary directory that is removed on exit. Keys are never written
beside the fixtures or committed. Regeneration is not byte-reproducible because
keys and ECDSA signatures are random; the names, validity, SPKI hashes, certificate
extensions, and expected verification results are reproducible.

These are certificate-chain/name/time tests, not live TLS handshakes: the
distrusted leaves cannot prove possession of their private keys. They do not
test native-root loading or deployed processes refreshing an in-memory store.
`ignore_unsafe_cert` is not used in any positive or distrust proof.

## Upstream license notice

The upstream public-key data is redistributed in the certificates above under
the rustls-webpki ISC-style license:

Copyright 2015 Brian Smith.

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHORS DISCLAIM ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
