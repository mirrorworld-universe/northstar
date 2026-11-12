#!/usr/bin/env python3
"""Verify the reviewed relay-only snapshot without private repository access."""
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / "northstar"
LOCK = BASE / "vrf-relay/source-lock.json"


def check():
    lock = json.loads(LOCK.read_text())
    assert lock["version"] == 1
    assert lock["repository"] == "https://github.com/mirrorworld-universe/vrf-oracle"
    revision = lock["revision"]
    assert len(revision) == 40 and all(c in "0123456789abcdef" for c in revision)
    expected = lock["published_sha256"]
    actual = {}
    for role in ("vrf-relay", "vrf-interface"):
        for path in (BASE / role).rglob("*"):
            if path.is_file() and (path.suffix == ".rs" or path.name == "Cargo.toml"):
                assert not path.is_symlink()
                actual[path.relative_to(BASE).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    assert actual == expected, "relay snapshot differs from reviewed source transformation"
    for name in ("checkpoint_anchor", "http_api", "signer_http", "signer_journal"):
        assert not (BASE / f"vrf-relay/src/{name}.rs").exists(), "private signer source must stay private"
    crypto = (BASE / "vrf-relay/src/crypto.rs").read_text()
    assert "pub struct OracleKey" not in crypto and "fn from_seed" not in crypto
    assert "EdVrfEdwards25519TaiSecretKey" not in crypto
    signer = (BASE / "vrf-relay/src/request_signer.rs").read_text()
    assert "struct RequestSigner" not in signer and "struct SignerConfig" not in signer
    assert "struct SignerPolicy" not in signer
    print("Reviewed relay-only snapshot verified; private signer/prover implementations excluded")


if __name__ == "__main__":
    check()
