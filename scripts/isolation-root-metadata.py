#!/usr/bin/env python3
"""Build the canonical isolation root metadata (root.json) from public keys only.

Inputs are OpenSSH public-key files (`.pub`, one line `<algorithm> <base64> [comment]`):
exactly three root keys (--root-key, in order R1 R2 R3) and the target key
(--target-key). Output is the canonical, UNSIGNED RootMetadata of duduclaw-isolation
update.rs:

  {expires_at_unix, frozen:false, revoked_key_ids:[], root_keys:[...], root_threshold:2,
   schema_version:1, target_key:{...}, version}

with key_id = SHA-256 of the public key blob. Rules (same as the Rust verifier,
trust::validate_root and sshsig::validate_key): roots are ssh-ed25519 or
sk-ssh-ed25519@openssh.com with application ssh:duduclaw-root, and at least one
root is a security key; the target is an sk-ssh-ed25519@openssh.com key with
application ssh:duduclaw-target; all four keys are distinct. ECDSA security keys
are refused until the verifier supports them (plan Q5).

Sign the output afterwards with the root threshold, e.g. R1 (touch + PIN) and R2
(key locations on stdin, one per line, R1 first: prompted on a terminal; never argv
or the environment):
  python3 -I scripts/sign-isolation-manifest.py --kind root --template root.unsigned.json \
      --output root.json --key-count 2 --ssh-keygen <Homebrew ssh-keygen>
The SHA-256 of the signed root.json (printed by that command, identical to the
one printed here) is the bootstrap root pin, release/bootstrap-root.sha256.

No private key or key handle is read by this tool.
"""
import argparse
import base64
import binascii
import hashlib
import importlib.util
from pathlib import Path
import struct
import time

ROOT = Path(__file__).resolve().parents[1]
ROOT_APPLICATION = "ssh:duduclaw-root"
TARGET_APPLICATION = "ssh:duduclaw-target"
ALGORITHMS = {"ssh-ed25519": "ed25519", "sk-ssh-ed25519@openssh.com": "sk_ed25519"}
# duduclaw-isolation MAX_KEY_BLOB_BYTES; a public key line is far below this.
MAX_KEY_BLOB_BYTES = 4096
MAX_PUBLIC_FILE_BYTES = 16 * 1024


def load_signer():
    spec = importlib.util.spec_from_file_location("isolation_signer", ROOT / "scripts/sign-isolation-manifest.py")
    signer = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(signer)
    return signer


def ssh_strings(blob):
    """Split an SSH wire blob into its length-prefixed strings; nothing may remain."""
    fields = []
    offset = 0
    while offset < len(blob):
        if offset + 4 > len(blob):
            raise ValueError("truncated public key blob")
        (length,) = struct.unpack(">I", blob[offset:offset + 4])
        offset += 4
        if offset + length > len(blob):
            raise ValueError("truncated public key blob")
        fields.append(blob[offset:offset + length])
        offset += length
    return fields


def parse_public_key(text, application):
    """One OpenSSH public-key line -> RootMetadata SigningKey (JSON form).

    `application` is the role's required security-key application; plain
    ssh-ed25519 is accepted only when `application` is the root application.
    """
    if text.endswith("\n"):
        text = text[:-1]
    if not text or "\n" in text or "\r" in text or not text.isascii():
        raise ValueError("public key file must hold exactly one ASCII line")
    parts = text.split(" ", 2)
    if len(parts) < 2 or parts[0] not in ALGORITHMS:
        raise ValueError("unsupported public key algorithm (ssh-ed25519 or sk-ssh-ed25519@openssh.com only)")
    try:
        blob = base64.b64decode(parts[1], validate=True)
    except (binascii.Error, ValueError) as error:
        raise ValueError("public key is not standard base64") from error
    if base64.b64encode(blob).decode("ascii") != parts[1] or not 0 < len(blob) <= MAX_KEY_BLOB_BYTES:
        raise ValueError("public key is not canonical base64 or is oversized")
    fields = ssh_strings(blob)
    if not fields or fields[0] != parts[0].encode("ascii"):
        raise ValueError("public key blob algorithm differs from the line")
    algorithm = ALGORITHMS[parts[0]]
    if algorithm == "ed25519":
        if len(fields) != 2 or len(fields[1]) != 32 or application != ROOT_APPLICATION:
            raise ValueError("plain ssh-ed25519 is only accepted as a root key (R2/R3)")
        key_application = None
    else:
        if len(fields) != 3 or len(fields[1]) != 32:
            raise ValueError("malformed sk-ssh-ed25519 public key")
        try:
            key_application = fields[2].decode("ascii")
        except UnicodeDecodeError as error:
            raise ValueError("security-key application is not ASCII") from error
        if key_application != application:
            raise ValueError(f"security-key application must be {application}")
    return {"algorithm": algorithm, "application": key_application,
            "key_id": list(hashlib.sha256(blob).digest()), "public_blob": list(blob)}


def build_root(root_keys, target_key, version, expires_at_unix, now=None):
    now = int(time.time()) if now is None else now
    if len(root_keys) != 3:
        raise ValueError("exactly three root keys (R1 R2 R3)")
    if not isinstance(version, int) or isinstance(version, bool) or not 1 <= version < 2 ** 63:
        raise ValueError("root metadata version must be a positive integer")
    if not isinstance(expires_at_unix, int) or isinstance(expires_at_unix, bool) \
            or not now < expires_at_unix < 2 ** 63:
        raise ValueError("root metadata must expire in the future")
    if not any(key["algorithm"] == "sk_ed25519" for key in root_keys):
        raise ValueError("at least one root key must be a security key (R1)")
    if target_key["algorithm"] != "sk_ed25519" or target_key["application"] != TARGET_APPLICATION:
        raise ValueError("the target key must be an sk-ssh-ed25519 key with application " + TARGET_APPLICATION)
    blobs = [bytes(key["public_blob"]) for key in [*root_keys, target_key]]
    if len(set(blobs)) != len(blobs):
        raise ValueError("root and target keys must all be distinct")
    return {"schema_version": 1, "version": version, "expires_at_unix": expires_at_unix, "root_threshold": 2,
            "root_keys": list(root_keys), "target_key": target_key, "revoked_key_ids": [], "frozen": False}


def read_public_file(path):
    with open(path, "rb") as source:
        raw = source.read(MAX_PUBLIC_FILE_BYTES + 1)
    if len(raw) > MAX_PUBLIC_FILE_BYTES:
        raise ValueError("public key file is too large")
    try:
        return raw.decode("ascii")
    except UnicodeDecodeError as error:
        raise ValueError("public key file is not ASCII") from error


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root-key", type=Path, action="append", required=True,
                        help="root public key (.pub); exactly three, in order R1 R2 R3")
    parser.add_argument("--target-key", type=Path, required=True, help="target public key (.pub)")
    parser.add_argument("--version", type=int, default=1, help="root metadata version (1 for the first root)")
    parser.add_argument("--expires-at-unix", type=int, required=True, help="root metadata expiry (Unix seconds)")
    parser.add_argument("--output", type=Path, required=True, help="new file; never overwritten")
    args = parser.parse_args()
    signer = load_signer()
    try:
        roots = [parse_public_key(read_public_file(path), ROOT_APPLICATION) for path in args.root_key]
        target = parse_public_key(read_public_file(args.target_key), TARGET_APPLICATION)
        document = build_root(roots, target, args.version, args.expires_at_unix)
    except (OSError, ValueError) as error:
        parser.error(str(error))
    payload = signer.canonical_bytes(document)
    with args.output.open("xb") as output:
        output.write(payload)
    print(hashlib.sha256(payload).hexdigest())


if __name__ == "__main__":
    main()
