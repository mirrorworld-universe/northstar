"""Read-only transport checks; they do not qualify a callback or settlement."""

from http.client import HTTPConnection, HTTPException
import json
from pathlib import Path
import sys


def request(port, method, path, *, body=None, headers=None):
    connection = HTTPConnection("127.0.0.1", port, timeout=3)
    try:
        connection.request(method, path, body=body, headers=headers or {})
        with connection.getresponse() as response:
            if response.status != 200:
                raise ValueError("unexpected status")
            return json.loads(response.read(16384))
    finally:
        connection.close()


def rpc(port, method, params):
    return request(
        port,
        "POST",
        "/",
        body=json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}),
        headers={"Content-Type": "application/json"},
    )["result"]


def signer_info(bearer_file, public_key, policy_digest=None):
    token = Path(bearer_file).read_bytes().removesuffix(b"\n").decode("ascii")
    info = request(9030, "GET", "/v1/info", headers={"Authorization": f"Bearer {token}"})
    return (
        info["version"] == 1
        and info["public_key"] == public_key
        and info["commitment"] == "finalized_portal_checkpoint_membership"
        and info["arbitrary_alpha"] is False
        and (policy_digest is None or info["policy_sha256"] == policy_digest)
    )


def validator():
    config = json.loads(Path("/etc/northstar/relay.json").read_bytes())
    remote = config["signer"]
    if not signer_info(
        remote["bearer_token_file"], config["worker"]["vrf_public_key"], remote["policy_sha256"]
    ) or rpc(8899, "getHealth", []) != "ok":
        return False
    slot = rpc(8910, "getSlot", [{"commitment": "confirmed"}])
    return type(slot) is int and slot >= 0


def signer():
    config = json.loads(Path("/etc/northstar/signer.json").read_bytes())
    return signer_info(config["bearer_token_file"], config["policy"]["vrf_public_key"])


def main(role):
    try:
        if {"validator": validator, "signer": signer}[role]():
            return 0
    except (OSError, HTTPException, KeyError, TypeError, ValueError):
        pass
    # Docker retains probe output. Never include request headers or config contents.
    print("Northstar readiness probe failed", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1]))
