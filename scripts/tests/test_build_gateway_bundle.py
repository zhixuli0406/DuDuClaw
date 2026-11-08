"""Gateway bundle layout and metadata, without Apple tooling or a Rust build."""
import importlib.util
from pathlib import Path
import plistlib
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('builder', ROOT / 'scripts/macos/build-gateway-bundle.py')
builder = importlib.util.module_from_spec(spec)
spec.loader.exec_module(builder)


class BundleTests(unittest.TestCase):
    def test_ce_and_pro_layout(self):
        for name in ('duduclaw', 'duduclaw-pro'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                exe = root / name
                exe.write_bytes(b'fake Mach-O')
                icon = root / 'icon.icns'
                icon.write_bytes(b'icns')
                app = builder.build_bundle(exe, name, '1.81.0', root / 'out', icon)
                info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
                self.assertEqual(info['CFBundleIdentifier'], 'ai.duduclaw.gateway')
                self.assertEqual(info['CFBundleExecutable'], name)
                self.assertEqual(info['CFBundleShortVersionString'], '1.81.0')
                self.assertEqual(info['CFBundleVersion'], '1.81.0')
                self.assertEqual(info['CFBundlePackageType'], 'APPL')
                self.assertEqual(info['CFBundleInfoDictionaryVersion'], '6.0')
                self.assertEqual(info['LSMinimumSystemVersion'], '11.0')
                self.assertIs(info['LSUIElement'], True)
                for key in ('CFBundleName', 'CFBundleDisplayName'):
                    self.assertEqual(info[key], 'DuDuClaw Gateway')
                for scope in ('AppleEvents', 'DesktopFolder', 'DocumentsFolder', 'DownloadsFolder', 'RemovableVolumes', 'NetworkVolumes', 'LocalNetwork'):
                    self.assertTrue(info[f'NS{scope}UsageDescription'].isascii())
                self.assertEqual((app / 'Contents/MacOS' / name).read_bytes(), exe.read_bytes())
                self.assertEqual((app / 'Contents/MacOS' / name).stat().st_mode & 0o777, 0o755)
                self.assertEqual((app / 'Contents/Resources/AppIcon.icns').read_bytes(), b'icns')
                with self.assertRaises(FileExistsError):
                    builder.build_bundle(exe, name, '1.81.0', root / 'out')

    def test_invalid_versions_do_not_create_bundle(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            exe = root / 'exe'
            exe.write_bytes(b'x')
            for version in ('v1.2.3', '1.2', '1.2.3-rc1', '01.2.3', '../1.2.3', '1.2.3\n'):
                with self.subTest(version=version), self.assertRaises(ValueError):
                    builder.build_bundle(exe, 'duduclaw', version, root / 'out')
            self.assertFalse((root / 'out').exists())
            app = builder.build_bundle(exe, 'duduclaw', '0.0.0', root / 'out')
            self.assertFalse((app / 'Contents/Resources/AppIcon.icns').exists())


if __name__ == '__main__':
    unittest.main()
