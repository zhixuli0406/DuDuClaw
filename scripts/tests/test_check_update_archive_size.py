"""Legacy-updater size gate and release wiring (no Rust build required)."""
import gzip
import importlib.util
import io
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/check_update_archive_size.py"
spec = importlib.util.spec_from_file_location("archive_size", SCRIPT)
checker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(checker)


class UpdateArchiveSizeTests(unittest.TestCase):
    def test_strict_legacy_boundary(self):
        for name in checker.BINARY_NAMES:
            with self.subTest(name=name):
                self.assertTrue(checker.check_member(name, 209_715_199, True))
                for size in (209_715_200, 210_561_920):
                    with self.assertRaisesRegex(ValueError, "<=1.79.2"):
                        checker.check_member(name, size, True)

    def test_actual_tar_and_zip_and_missing_binary(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            tar = root / "release.tar.gz"
            with tarfile.open(tar, "w:gz") as archive:
                entry = tarfile.TarInfo("./duduclaw")
                entry.size = 4
                archive.addfile(entry, io.BytesIO(b"test"))
            checker.check_archive(tar)
            with patch.object(checker, "LEGACY_BINARY_LIMIT", 4):
                with self.assertRaisesRegex(ValueError, "<=1.79.2"):
                    checker.check_archive(tar)
            zip_path = root / "release.zip"
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("duduclaw.exe", b"test")
            checker.check_archive(zip_path)
            with patch.object(checker, "LEGACY_BINARY_LIMIT", 4):
                with self.assertRaisesRegex(ValueError, "<=1.79.2"):
                    checker.check_archive(zip_path)
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("python/helper.py", b"test")
            result = subprocess.run([sys.executable, str(SCRIPT), str(zip_path)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertIn("no duduclaw update binary", result.stderr)

    def test_nested_python_package_dir_ignored_tar_and_zip(self):
        with tempfile.TemporaryDirectory() as directory:
            tar = Path(directory) / "release.tar.gz"
            with tarfile.open(tar, "w:gz") as archive:
                entry = tarfile.TarInfo("duduclaw")
                entry.size = 4
                archive.addfile(entry, io.BytesIO(b"test"))
                for name in ("python", "python/duduclaw"):
                    d = tarfile.TarInfo(name)
                    d.type = tarfile.DIRTYPE
                    archive.addfile(d)
                f = tarfile.TarInfo("python/duduclaw/__init__.py")
                f.size = 1
                archive.addfile(f, io.BytesIO(b"#"))
            checker.check_archive(tar)
            result = subprocess.run([sys.executable, str(SCRIPT), str(tar)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            zip_path = Path(directory) / "release.zip"
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("duduclaw.exe", b"test")
                archive.writestr("python/duduclaw/", b"")
                archive.writestr("python/duduclaw/__init__.py", b"#")
            checker.check_archive(zip_path)

    def test_nested_dir_does_not_weaken_other_checks(self):
        self.assertFalse(checker.check_member("python/duduclaw", 0, False, True))
        self.assertFalse(checker.check_member("python/duduclaw/", 0, False, True))
        with self.assertRaisesRegex(ValueError, "archive root"):
            checker.check_member("python/duduclaw", 4, True, False)
        with self.assertRaisesRegex(ValueError, "regular file"):
            checker.check_member("duduclaw", 0, False, True)
        with self.assertRaisesRegex(ValueError, "regular file"):
            checker.check_member("./duduclaw/", 0, False, True)
        with self.assertRaisesRegex(ValueError, "unsafe"):
            checker.check_member("../python/duduclaw", 0, False, True)
        with self.assertRaisesRegex(ValueError, "<=1.79.2"):
            checker.check_member("duduclaw", checker.LEGACY_BINARY_LIMIT, True)
        with tempfile.TemporaryDirectory() as directory:
            tar = Path(directory) / "release.tar.gz"
            with tarfile.open(tar, "w:gz") as archive:
                for name in ("duduclaw", "python/duduclaw"):
                    entry = tarfile.TarInfo(name)
                    entry.size = 4
                    archive.addfile(entry, io.BytesIO(b"test"))
            with self.assertRaisesRegex(ValueError, "archive root"):
                checker.check_archive(tar)
            zip_path = Path(directory) / "release.zip"
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("duduclaw.exe", b"test")
                archive.writestr("python/duduclaw", b"test")
            with self.assertRaisesRegex(ValueError, "archive root"):
                checker.check_archive(zip_path)

    def test_symlink_binary_rejected(self):
        with self.assertRaisesRegex(ValueError, "regular file"):
            checker.check_member("duduclaw", 0, False)

    def test_unsafe_paths_and_nested_binaries_in_real_archives(self):
        names = (
            "/duduclaw", "../duduclaw", "safe/../duduclaw", "nested/duduclaw",
            "nested/./duduclaw", "C:/duduclaw", "C:\\duduclaw",
            "\\\\server\\share\\duduclaw", "nested\\duduclaw", "/other", "../other",
        )
        with tempfile.TemporaryDirectory() as directory:
            for name in names:
                for extension in ("tar.gz", "zip"):
                    with self.subTest(name=name, extension=extension):
                        path = Path(directory) / ("release." + extension)
                        if extension == "tar.gz":
                            with tarfile.open(path, "w:gz") as archive:
                                for member in ("duduclaw.exe", name):
                                    entry = tarfile.TarInfo(member)
                                    entry.size = 4
                                    archive.addfile(entry, io.BytesIO(b"test"))
                        else:
                            with zipfile.ZipFile(path, "w") as archive:
                                archive.writestr("duduclaw.exe", b"test")
                                archive.writestr(name, b"test")
                        with self.assertRaisesRegex(ValueError, "path|archive root"):
                            checker.check_archive(path)

    def test_real_tar_links_and_zip_symlink_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "release.tar.gz"
            for kind in (tarfile.SYMTYPE, tarfile.LNKTYPE):
                with self.subTest(kind=kind):
                    with tarfile.open(path, "w:gz") as archive:
                        entry = tarfile.TarInfo("duduclaw")
                        entry.type = kind
                        entry.linkname = "target"
                        archive.addfile(entry)
                    with self.assertRaisesRegex(ValueError, "regular file"):
                        checker.check_archive(path)
            path = Path(directory) / "release.zip"
            with zipfile.ZipFile(path, "w") as archive:
                entry = zipfile.ZipInfo("duduclaw.exe")
                entry.create_system = 3
                entry.external_attr = 0o120777 << 16
                archive.writestr(entry, "target")
            with self.assertRaisesRegex(ValueError, "regular file"):
                checker.check_archive(path)

    def test_truncated_archives_fail_cli(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            # A valid header promises more bytes than the tar body contains.
            entry = tarfile.TarInfo("duduclaw")
            entry.size = 4096
            broken_tar = gzip.compress(entry.tobuf() + b"short")
            valid_tar = root / "valid.tar.gz"
            with tarfile.open(valid_tar, "w:gz") as archive:
                entry.size = 4
                archive.addfile(entry, io.BytesIO(b"test"))
            zip_path = root / "valid.zip"
            with zipfile.ZipFile(zip_path, "w") as archive:
                archive.writestr("duduclaw.exe", b"test")
            for index, (extension, data) in enumerate((
                ("tar.gz", broken_tar),
                ("tar.gz", valid_tar.read_bytes()[:-8]),  # Missing gzip trailer.
                ("zip", zip_path.read_bytes()[:-22]),
            )):
                with self.subTest(index=index):
                    path = root / (str(index) + "." + extension)
                    path.write_bytes(data)
                    result = subprocess.run([sys.executable, str(SCRIPT), str(path)],
                                            capture_output=True, text=True)
                    self.assertEqual(result.returncode, 1)
                    self.assertIn("ERROR:", result.stderr)
                    self.assertNotIn("Traceback", result.stderr)

    def test_release_profiles_and_archive_gates(self):
        for path in ("Cargo.toml", "src-tauri/Cargo.toml"):
            import tomllib
            manifest = tomllib.loads((ROOT / path).read_text())
            self.assertEqual(manifest["profile"]["release"]["strip"], "symbols")
        for path in ("release.yml", "desktop-release.yml", "release-cache-warm.yml"):
            workflow = (ROOT / ".github/workflows" / path).read_text()
            self.assertIn("startsWith(matrix.os, 'windows') && 'none' || 'symbols'", workflow)
        workflow = (ROOT / ".github/workflows/release.yml").read_text()
        self.assertIn("check_update_archive_size.py ${{ matrix.artifact }}.tar.gz", workflow)
        self.assertIn("check_update_archive_size.py ${{ matrix.artifact }}.zip", workflow)
        release = (ROOT / "scripts/release.sh").read_text()
        self.assertIn('check_update_archive_size.py" "$tar_path" || exit 1', release)
        self.assertIn('"$strip_tool" --strip-all', release)
        self.assertIn("CARGO_PROFILE_RELEASE_STRIP=symbols cargo build", release)


if __name__ == "__main__":
    unittest.main()
