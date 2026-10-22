"""Regenerate public-only TLS test certificates (Python 3 stdlib, OpenSSL >= 3.4)."""

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import urllib.request


COMMIT = "8ecdcc76f56b68fda67a6adf7976f5eed5006a4c"
SPKIS = {
    "EMDVTLSCAG3B": "2d5f988ee3ceaf696845a34299ca3e05816429cdb5de92427889533175f97b95",
    "EMDVTLSCAG3C": "616e9a92539c40f454813f7fb8ae4a6db719498495215774f02d8b8b5a615d36",
    "EMEVTLSCAG3B": "bdf5ab18303069ebc3e3ddbb0fc0613693acef01b2fb7be1a0624aab60fbff0d",
    "EMEVTLSCAG3C": "b941c7577b918f7b9a9374aff3b02d8eda92545c86adb5b5268d1e121fe58ce2",
    "EMOVCSCAG3B": "e8263cea915a92109a29fb497105dceeeb58fa099c2552c4082aff1e1a9a33b2",
    "EMOVCSCAG3C": "09f75d128061fcdc6d31d8317ce3fe0aea4dab2c66777ce548858dd23f763ce7",
    "EMOVTLSCAG3B": "7f09de2d155cc9fbc2dc4585f2c134ea3fc04ea9d9b3e31fa2d6bf9fdc0a02a2",
    "EMOVTLSCAG3C": "6fc13706463e00b2cd2ae357f86de2441e59d501e322c7a02a531a9871e450f6",
}
OUTPUT = Path(__file__).resolve().parent
VALIDITY = ["-not_before", "20260101000000Z", "-not_after", "20300101000000Z"]


def openssl(*args):
    result = subprocess.run(
        ["openssl", *map(str, args)], capture_output=True,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == "nt" else 0,
    )
    if result.returncode:
        raise RuntimeError(result.stderr.decode(errors="replace"))
    return result.stdout


def generate(directory):
    root_key = directory / "root-key.pem"
    leaf_key = directory / "leaf-key.pem"
    leaf_public_key = directory / "leaf-public.pem"
    root = directory / "root.pem"
    extensions = directory / "extensions.cnf"
    extensions.write_text(
        "[root]\nbasicConstraints=critical,CA:TRUE\n"
        "keyUsage=critical,keyCertSign,cRLSign\nsubjectKeyIdentifier=hash\n"
        "[leaf]\nbasicConstraints=critical,CA:FALSE\n"
        "keyUsage=critical,digitalSignature\nextendedKeyUsage=serverAuth\n"
        "subjectAltName=DNS:tls-fixture.invalid\n",
        encoding="ascii",
    )
    for key in (root_key, leaf_key):
        openssl("genpkey", "-algorithm", "EC", "-pkeyopt", "ec_paramgen_curve:P-256", "-out", key)
    openssl(
        "x509", "-new", "-key", root_key, "-subj", "/CN=Synthetic TLS fixture root",
        "-set_serial", "1", *VALIDITY, "-extfile", extensions, "-extensions", "root", "-out", root,
    )
    openssl("x509", "-in", root, "-outform", "DER", "-out", OUTPUT / "root.der")
    openssl("pkey", "-in", leaf_key, "-pubout", "-out", leaf_public_key)
    common = [
        "-subj", "/CN=tls-fixture.invalid", *VALIDITY,
        "-extfile", extensions, "-extensions", "leaf", "-outform", "DER",
    ]
    openssl(
        "x509", "-new", "-force_pubkey", leaf_public_key, "-CA", root, "-CAkey", root_key,
        "-set_serial", "2", *common, "-out", OUTPUT / "valid.der",
    )
    openssl(
        "x509", "-new", "-key", leaf_key, "-set_serial", "3",
        *common, "-out", OUTPUT / "self-signed.der",
    )
    for serial, (name, expected_hash) in enumerate(SPKIS.items(), start=10):
        url = f"https://raw.githubusercontent.com/rustls/webpki/{COMMIT}/src/data/distrusted/{name}.spki.der"
        with urllib.request.urlopen(url, timeout=30) as response:
            spki = response.read()
        if hashlib.sha256(spki).hexdigest() != expected_hash:
            raise ValueError(f"Unexpected upstream SPKI digest: {name}")
        # webpki stores the SPKI SEQUENCE contents, not its outer tag/length.
        length = len(spki)
        encoded_length = length.to_bytes((length.bit_length() + 7) // 8, "big")
        der_length = bytes([length]) if length < 128 else bytes([0x80 | len(encoded_length)]) + encoded_length
        public_key = directory / f"{name}.pem"
        wrapped = directory / "spki.der"
        wrapped.write_bytes(b"\x30" + der_length + spki)
        openssl("pkey", "-pubin", "-inform", "DER", "-in", wrapped, "-out", public_key)
        openssl(
            "x509", "-new", "-force_pubkey", public_key, "-CA", root, "-CAkey", root_key,
            "-set_serial", str(serial), *common, "-out", OUTPUT / f"{name}.der",
        )


if __name__ == "__main__":
    # Keys exist only in this temporary directory and are removed on exit.
    with tempfile.TemporaryDirectory(prefix="komari-tls-fixtures-") as temporary:
        generate(Path(temporary))
    print("Generated 11 public certificates; temporary synthetic keys removed.")
