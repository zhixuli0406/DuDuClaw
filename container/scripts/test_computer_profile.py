"""Profile archive contract; runs without Docker using temporary directories."""

import importlib.util
import io
from pathlib import Path
import tarfile
import tempfile
import unittest


spec = importlib.util.spec_from_file_location(
    "computer_profile", Path(__file__).with_name("duduclaw_profile.py")
)
profile = importlib.util.module_from_spec(spec)
spec.loader.exec_module(profile)


def archive(name, content=b"cookie", kind=tarfile.REGTYPE):
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode="w") as tar:
        info = tarfile.TarInfo(name)
        info.type = kind
        info.linkname = "/etc/passwd"
        info.size = len(content) if kind == tarfile.REGTYPE else 0
        tar.addfile(info, io.BytesIO(content) if info.size else None)
    return out.getvalue()


class ProfileTests(unittest.TestCase):
    def test_only_login_files_roundtrip(self):
        with tempfile.TemporaryDirectory() as root:
            source, dest = Path(root) / "source", Path(root) / "dest"
            (source / "Default/Local Storage/leveldb").mkdir(parents=True)
            (source / "Default/IndexedDB/site").mkdir(parents=True)
            for name in ("Cookies", "Cookies-journal", "Preferences", "Login Data"):
                (source / "Default" / name).write_bytes(name.encode())
            (source / "Default/Local Storage/leveldb/0001.ldb").write_bytes(b"local")
            (source / "Default/IndexedDB/site/0002.ldb").write_bytes(b"indexed")
            blob = profile.export_archive(source, 1024 * 1024)
            profile.import_archive(blob, dest, 1024 * 1024)
            self.assertEqual((dest / "Default/Cookies").read_bytes(), b"Cookies")
            self.assertEqual((dest / "Default/IndexedDB/site/0002.ldb").read_bytes(), b"indexed")
            self.assertFalse((dest / "Default/Preferences").exists())
            self.assertFalse((dest / "Default/Login Data").exists())

    def test_untrusted_members_refused_before_changing_destination(self):
        bad_names = ("../outside", "/Default/Cookies", "Default/../Cookies",
                     "Default/Preferences", "Default/Local Storage/leveldb/../../escape")
        for name in bad_names:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as root:
                dest = Path(root) / "profile"
                (dest / "Default").mkdir(parents=True)
                (dest / "Default/Cookies").write_bytes(b"old")
                with self.assertRaises(ValueError):
                    profile.import_archive(archive(name), dest, 1024 * 1024)
                self.assertEqual((dest / "Default/Cookies").read_bytes(), b"old")

    def test_links_and_specials_refused(self):
        for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE, tarfile.CHRTYPE):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as root:
                with self.assertRaises(ValueError):
                    profile.import_archive(archive("Default/Cookies", kind=kind), Path(root) / "p", 1024 * 1024)

    def test_export_symlink_and_hardlink_refused(self):
        with tempfile.TemporaryDirectory() as root:
            source = Path(root) / "p"
            (source / "Default").mkdir(parents=True)
            (source / "Default/Cookies").symlink_to("/etc/passwd")
            with self.assertRaises(ValueError):
                profile.export_archive(source, 1024 * 1024)
            (source / "Default/Cookies").unlink()
            outside = Path(root) / "secret"
            outside.write_bytes(b"secret")
            (source / "Default/Cookies").hardlink_to(outside)
            with self.assertRaises(ValueError):
                profile.export_archive(source, 1024 * 1024)

    def test_size_limit_applies_to_archive_and_data(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(ValueError):
                profile.import_archive(archive("Default/Cookies"), Path(root) / "p", 100)
            source = Path(root) / "source"
            (source / "Default").mkdir(parents=True)
            (source / "Default/Cookies").write_bytes(b"x" * 200)
            with self.assertRaises(ValueError):
                profile.export_archive(source, 100)

    def test_duplicate_member_refused(self):
        out = io.BytesIO()
        with tarfile.open(fileobj=out, mode="w") as tar:
            for _ in range(2):
                tar.addfile(tarfile.TarInfo("Default/Cookies"), io.BytesIO())
        with tempfile.TemporaryDirectory() as root, self.assertRaises(ValueError):
            profile.import_archive(out.getvalue(), Path(root) / "p", 1024 * 1024)


if __name__ == "__main__":
    unittest.main()
