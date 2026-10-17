"""Security vendor checks must reject source and provenance drift."""

import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).resolve().parents[1] / "verify_security_vendor.py"
SPEC = importlib.util.spec_from_file_location("verify_security_vendor", SCRIPT)
VERIFY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(VERIFY)


class SecurityVendorTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.vendor = self.root / "vendor"
        self.vendor.mkdir()
        self.originals = {
            "Cargo.toml": b'[dependencies.zip]\nversion = "8.6"\n',
            "src/lib.rs": b"// upstream implementation\n",
            "LICENSE-MIT.md": b"upstream license\n",
        }
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w:gz") as output:
            for name, body in self.originals.items():
                info = tarfile.TarInfo("fixture-1.0.0/" + name)
                info.size = len(body)
                output.addfile(info, io.BytesIO(body))
                target = self.vendor / name
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(body)
        data = archive.getvalue()
        (self.root / "fixture-1.0.0.crate").write_bytes(data)
        checksum = hashlib.sha256(data).hexdigest()
        metadata = {
            "base_version": "1.0.0",
            "base_crate_sha256": checksum,
            "replacements": {
                "Cargo.toml": [['version = "8.6"', 'version = "9.0.4"']]
            },
        }
        provenance = json.dumps(metadata).encode()
        (self.vendor / "SECURITY-PATCH.json").write_bytes(provenance)
        (self.vendor / "Cargo.toml").write_bytes(
            self.originals["Cargo.toml"].replace(b'"8.6"', b'"9.0.4"')
        )
        self.packages = {"fixture": (self.vendor, "1.0.0", checksum)}
        self.pins = {"fixture": hashlib.sha256(provenance).hexdigest()}

    def verify(self):
        with patch.object(VERIFY, "PACKAGES", self.packages), patch.object(
            VERIFY, "PROVENANCE_SHA256", self.pins
        ), contextlib.redirect_stdout(io.StringIO()):
            VERIFY.verify("fixture", self.root)

    def test_explicit_manifest_only_patch_preserves_source_and_license(self):
        self.verify()

    def test_changed_source_is_rejected(self):
        (self.vendor / "src/lib.rs").write_bytes(b"changed implementation\n")
        with self.assertRaisesRegex(ValueError, "unexpected change: src/lib.rs"):
            self.verify()

    def test_missing_license_is_rejected(self):
        (self.vendor / "LICENSE-MIT.md").unlink()
        with self.assertRaisesRegex(ValueError, "required upstream source or license is missing"):
            self.verify()

    def test_changed_patch_specification_is_rejected(self):
        (self.vendor / "SECURITY-PATCH.json").write_bytes(b"{}")
        with self.assertRaisesRegex(ValueError, "unexpected security patch specification"):
            self.verify()

    def test_changed_official_archive_is_rejected(self):
        (self.root / "fixture-1.0.0.crate").write_bytes(b"different source")
        with self.assertRaisesRegex(ValueError, "upstream archive checksum mismatch"):
            self.verify()


if __name__ == "__main__":
    unittest.main()
