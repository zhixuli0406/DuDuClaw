"""Offline app-archive and workflow contracts; never invoke Apple signing tools."""
import importlib.util
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
from unittest.mock import patch
import zipfile

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / 'scripts' / (name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


marker = load('verify_release_marker')
size = load('check_update_archive_size')


class AppArchiveTests(unittest.TestCase):
    def fixture(self, path, name='duduclaw', extra=(), payload=None):
        with zipfile.ZipFile(path, 'w') as z:
            binary = zipfile.ZipInfo(f'DuDuClaw Gateway.app/Contents/MacOS/{name}')
            binary.external_attr = 0o100755 << 16
            z.writestr(binary, payload or marker.HEADER + b'1.81.0' + marker.SUFFIX)
            z.writestr('DuDuClaw Gateway.app/Contents/Info.plist', b'plist')
            for entry in extra:
                is_dir = isinstance(entry, zipfile.ZipInfo) and entry.is_dir()
                z.writestr(entry, b'' if is_dir else b'x')

    def test_editions_and_architectures(self):
        with tempfile.TemporaryDirectory() as tmp:
            for name in ('duduclaw', 'duduclaw-pro'):
                for arch in ('arm64', 'x64'):
                    path = Path(tmp) / f'{name}-darwin-{arch}.app.zip'
                    self.fixture(path, name)
                    size.check_archive(path)
                    marker.verify_archive(path, '1.81.0')
                    with self.assertRaises(ValueError):
                        marker.verify_archive(path, '1.80.0')
                    with patch.object(size._bundle, 'MAX_BUNDLE_BYTES', 1):
                        with self.assertRaisesRegex(ValueError, 'size'):
                            size.check_archive(path)
                    with patch.object(size._bundle, 'MAX_EXECUTABLE_BYTES', 1):
                        with self.assertRaisesRegex(ValueError, 'size'):
                            size.check_archive(path)

    def test_reject_unsafe_extra_executables_and_wrong_edition(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'duduclaw-darwin-arm64.app.zip'
            link = zipfile.ZipInfo('DuDuClaw Gateway.app/Contents/Resources/link')
            link.external_attr = 0o120777 << 16
            executable = zipfile.ZipInfo('DuDuClaw Gateway.app/Contents/Resources/extra')
            executable.external_attr = 0o100755 << 16
            for extra in (('../escape',), ('DuDuClaw Gateway.app/../escape',),
                          ('duduclaw',), ('DuDuClaw Gateway.app/Contents/MacOS/other',),
                          (link,), (executable,)):
                self.fixture(path, extra=extra)
                for gate in (lambda: size.check_archive(path), lambda: marker.verify_archive(path, '1.81.0')):
                    with self.subTest(extra=extra), self.assertRaises(ValueError):
                        gate()
            self.fixture(path, 'duduclaw-pro')
            with self.assertRaises(ValueError):
                marker.verify_archive(path, '1.81.0')

    def test_reject_special_permission_and_writable_modes(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'duduclaw-darwin-arm64.app.zip'
            for mode in (0o104644, 0o102644, 0o101644, 0o100664, 0o100646, 0o040777, 0o042755):
                entry = zipfile.ZipInfo('DuDuClaw Gateway.app/Contents/Resources/' +
                                        ('dir/' if mode & 0o040000 else 'file'))
                entry.external_attr = mode << 16
                self.fixture(path, extra=(entry,))
                for gate in (lambda: size.check_archive(path), lambda: marker.verify_archive(path, '1.81.0')):
                    with self.subTest(mode=oct(mode)), self.assertRaises(ValueError):
                        gate()
            # Case-insensitive duplicate of Info.plist.
            self.fixture(path, extra=('DuDuClaw Gateway.app/contents/info.plist',))
            with self.assertRaises(ValueError):
                size.check_archive(path)
            # The executable itself may not be setuid or writable by others.
            for mode in (0o104755, 0o100775):
                with zipfile.ZipFile(path, 'w') as z:
                    binary = zipfile.ZipInfo('DuDuClaw Gateway.app/Contents/MacOS/duduclaw')
                    binary.external_attr = mode << 16
                    z.writestr(binary, marker.HEADER + b'1.81.0' + marker.SUFFIX)
                    z.writestr('DuDuClaw Gateway.app/Contents/Info.plist', b'plist')
                with self.subTest(exe_mode=oct(mode)), self.assertRaises(ValueError):
                    size.check_archive(path)


class SigningContractTests(unittest.TestCase):
    def test_dry_run_and_missing_secrets(self):
        with tempfile.TemporaryDirectory() as tmp:
            app = Path(tmp) / 'DuDuClaw Gateway.app'
            app.mkdir()
            exe = Path(tmp) / 'duduclaw'
            exe.write_text('never execute me')
            env = {k: v for k, v in os.environ.items() if not k.startswith(('APPLE_', 'DUDUCLAW_NOTARY_'))}
            cmd = ['bash', str(ROOT / 'scripts/macos/sign-gateway.sh'), '--dry-run', '--app', str(app), '--exe', str(exe), '--out', str(Path(tmp) / 'release.app.zip')]
            result = subprocess.run(cmd, env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('APPLE_SIGNING_IDENTITY', result.stderr)
            env['APPLE_SIGNING_IDENTITY'] = 'Developer ID fixture'
            result = subprocess.run(cmd, env=env, capture_output=True, text=True)
            self.assertNotEqual(result.returncode, 0)
            for args in (['--keychain-profile', 'fixture'], []):
                if not args:
                    env.update(APPLE_ID='private-id', APPLE_PASSWORD='private-password', APPLE_TEAM_ID='7469HYQ6HH')
                result = subprocess.run(cmd + args, env=env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn('notarytool submit', result.stdout)
                self.assertIn('stapler staple', result.stdout)
                self.assertIn('--noextattr --noqtn --keepParent', result.stdout)
                self.assertNotIn('--deep', result.stdout)
                self.assertNotIn('private-password', result.stdout)
            result = subprocess.run(['bash', str(ROOT / 'scripts/macos/verify-gateway-signature.sh'), '--dry-run', '--app', str(app), '--exe', str(exe), '--tag', 'v1.81.0'], capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('7469HYQ6HH', result.stdout)
            self.assertIn('stapler validate', result.stdout)
            self.assertIn('--strict --deep', result.stdout)

    def test_pro_darwin_missing_identity_skips_both_assets(self):
        release = (ROOT / 'scripts/release.sh').read_text()
        start = release.index('package_and_upload_pro_bin() {')
        function = release[start:release.index('\n}\n', start) + 3]
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            binary = root / 'darwin-arm64/duduclaw-pro'
            binary.parent.mkdir()
            binary.write_bytes(marker.HEADER + b'1.81.0' + marker.SUFFIX)
            env = {k: v for k, v in os.environ.items() if not k.startswith(('APPLE_', 'DUDUCLAW_NOTARY_'))}
            command = ('set -euo pipefail\nPRO_GAPS=()\nPRO_BIN_BUCKET=unused\n'
                       + function + '\nif package_and_upload_pro_bin darwin-arm64 "$1" 1.81.0; then exit 9; fi\n'
                       + 'printf "%s\\n" "${PRO_GAPS[@]}"')
            result = subprocess.run(['bash', '-c', command, 'fixture', str(root)], cwd=ROOT,
                                    env=env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            self.assertIn('signing/notarization failed', result.stdout)
            self.assertFalse(list(root.glob('*.tar.gz')))
            self.assertFalse(list(root.glob('*.app.zip')))

    def test_workflow_bundle_delivery(self):
        workflow = (ROOT / '.github/workflows/release.yml').read_text()
        desktop = (ROOT / '.github/workflows/desktop-release.yml').read_text()
        pin = re.search(r'apple-actions/import-codesign-certs@([0-9a-f]{40})', desktop).group(1)
        self.assertIn('apple-actions/import-codesign-certs@' + pin, workflow)
        for script in ('build-gateway-bundle.py', 'sign-gateway.sh', 'verify-gateway-signature.sh'):
            self.assertIn(script, workflow)
        self.assertLess(workflow.index('Check binary is portable (macOS)'), workflow.index('Import macOS gateway'))
        self.assertLess(workflow.index('verify-gateway-signature.sh'), workflow.index('name: Package (macOS)'))
        self.assertIn('matrix.artifact }}.app.zip.sig', workflow)
        self.assertIn('for f in duduclaw-*.tar.gz duduclaw-*.zip', workflow)
        self.assertIn('            duduclaw-*.zip', workflow)
        for secret in ('APPLE_CERTIFICATE', 'APPLE_CERTIFICATE_PASSWORD', 'APPLE_SIGNING_IDENTITY', 'APPLE_ID', 'APPLE_PASSWORD', 'APPLE_TEAM_ID'):
            self.assertIn('${{ secrets.' + secret + ' }}', workflow)
        self.assertIn('::error::$key secret is required', workflow)


if __name__ == '__main__':
    unittest.main()
