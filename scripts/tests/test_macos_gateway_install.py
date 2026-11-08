"""Exercise the release installer with local fixtures and no network or builds."""
import hashlib
import io
import os
from pathlib import Path
import subprocess
import sys
import stat
import tarfile
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]
# Source definitions only; never invoke the interactive/source-build entry point.
INSTALLER = (ROOT / 'scripts/install.sh').read_text().rsplit('\nmain "$@"', 1)[0]
STUBS = r'''
curl() {
  local dest=""
  while [ "$#" -gt 0 ]; do
    if [ "$1" = '-o' ]; then dest="$2"; shift 2; else shift; fi
  done
  cp "$FIXTURE/app.zip" "$dest"
  printf '%s' "$HTTP_STATUS"
}
download() {
  case "$1" in
    *.sha256) cp "$FIXTURE/$ARCHIVE.sha256" "$2" ;;
    *.tar.gz) cp "$FIXTURE/bare.tar.gz" "$2" ;;
    *) return 1 ;;
  esac
}
python3() { echo 'installer must not invoke Python' >&2; return 99; }
codesign_fixture() {
  printf '%s\n' "$@" > "$FIXTURE/codesign-args"
  [ "$SIGNATURE_FAILURE" != 1 ]
}
mv() {
  if [ "$MOVE_FAILURE" = app ]; then
    case "$1" in */.duduclaw-install.*/'DuDuClaw Gateway.app') return 1 ;; esac
  fi
  if [ "$MOVE_FAILURE" = link ] && [ "$1" = -f ]; then return 1; fi
  command mv "$@"
}
install_from_release "$TARGET"
'''


class InstallTests(unittest.TestCase):
    def fixtures(self, root, bad_checksum=False):
        fixture = root / 'fixtures'
        fixture.mkdir()
        payload = b'gateway fixture'
        with zipfile.ZipFile(fixture / 'app.zip', 'w') as z:
            entry = zipfile.ZipInfo('DuDuClaw Gateway.app/Contents/MacOS/duduclaw')
            entry.external_attr = 0o100755 << 16
            z.writestr(entry, payload)
            z.writestr('DuDuClaw Gateway.app/Contents/Info.plist', b'fixture')
        with tarfile.open(fixture / 'bare.tar.gz', 'w:gz') as tar:
            entry = tarfile.TarInfo('duduclaw')
            entry.size = len(payload)
            entry.mode = 0o755
            tar.addfile(entry, io.BytesIO(payload))
        for ext, file in (('app.zip', 'app.zip'), ('tar.gz', 'bare.tar.gz')):
            digest = '0' * 64 if bad_checksum else hashlib.sha256((fixture / file).read_bytes()).hexdigest()
            for target in ('duduclaw-darwin-arm64', 'duduclaw-linux-x64'):
                (fixture / f'{target}.{ext}.sha256').write_text(f'{digest}  {target}.{ext}\n')
        return fixture

    def run_install(self, root, fixture, status='200', target='duduclaw-darwin-arm64',
                    signature_failure=False, move_failure='', real_codesign=False):
        ext = 'app.zip' if status == '200' and 'darwin' in target else 'tar.gz'
        env = dict(os.environ, HOME=str(root / 'home'), DUDUCLAW_HOME=str(root / 'cli'),
                   DUDUCLAW_APP_DIR=str(root / 'apps'), DUDUCLAW_VERSION='1.81.0',
                   FIXTURE=str(fixture), HTTP_STATUS=status, TARGET=target,
                   ARCHIVE=f'{target}.{ext}', SIGNATURE_FAILURE=str(int(signature_failure)),
                   MOVE_FAILURE=move_failure)
        installer = INSTALLER if real_codesign else INSTALLER.replace(
            '/usr/bin/codesign --verify', 'codesign_fixture --verify')
        return subprocess.run(['bash', '-c', installer + '\n' + STUBS], env=env,
                              capture_output=True, text=True)

    def test_checksum_mismatch_is_fatal_on_mac_and_linux(self):
        for target in ('duduclaw-darwin-arm64', 'duduclaw-linux-x64'):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root, bad_checksum=True)
                result = self.run_install(root, fixture, target=target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Checksum verification failed', result.stderr)
                self.assertFalse((root / 'cli/bin/duduclaw').exists())

    def test_404_falls_back_but_other_errors_are_fatal(self):
        for status in ('404', '500'):
            with self.subTest(status=status), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                result = self.run_install(root, fixture, status)
                self.assertEqual(result.returncode == 0, status == '404', result.stderr)
                self.assertEqual((root / 'cli/bin/duduclaw').exists(), status == '404')
                self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_install_then_atomic_replacement_keeps_cli_symlink(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            for round in range(2):
                result = self.run_install(root, fixture)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                link = root / 'cli/bin/duduclaw'
                self.assertTrue(link.is_symlink())
                self.assertEqual(link.resolve(), (root / 'apps/DuDuClaw Gateway.app/Contents/MacOS/duduclaw').resolve())
                self.assertEqual(link.read_bytes(), b'gateway fixture')
                self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))
                args = (fixture / 'codesign-args').read_text().splitlines()
                self.assertEqual(args[:2], ['--verify', '--strict'])
                self.assertIn('.duduclaw-install.', args[2])
                link.write_bytes(b'old binary')


    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_bundle_helper_runs_in_posix_shell_without_python(self):
        helper = INSTALLER.split('install_macos_bundle() (', 1)[1].split(
            '# ---------------------------------------------------------------------------', 1)[0]
        helper = 'install_macos_bundle() (' + helper
        helper = helper.replace('/usr/bin/codesign --verify', 'codesign_fixture --verify')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            script = """set -eu
fatal() { echo "$*" >&2; exit 1; }
python3() { exit 99; }
codesign_fixture() { [ "$1" = --verify ] && [ "$2" = --strict ]; }
""" + helper + '\ninstall_macos_bundle "$1" "$2" "$3"\n'
            result = subprocess.run(['/bin/sh', '-c', script, 'fixture',
                str(fixture / 'app.zip'), str(root / 'apps'), str(root / 'cli/bin')],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            link = root / 'cli/bin/duduclaw'
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.read_bytes(), b'gateway fixture')

    def rewrite_zip(self, fixture, entries):
        with zipfile.ZipFile(fixture / 'app.zip', 'w') as archive:
            for name, mode, body in entries:
                entry = zipfile.ZipInfo(name)
                entry.create_system = 3
                entry.external_attr = mode << 16
                archive.writestr(entry, body)
        digest = hashlib.sha256((fixture / 'app.zip').read_bytes()).hexdigest()
        (fixture / 'duduclaw-darwin-arm64.app.zip.sha256').write_text(
            f'{digest}  duduclaw-darwin-arm64.app.zip\n')

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_unsafe_listings_are_rejected_before_extracting(self):
        expected = 'DuDuClaw Gateway.app/Contents/MacOS/duduclaw'
        invalid = [
            '/outside', '../outside', 'DuDuClaw Gateway.app/../outside',
            'Other.app/file', 'DuDuClaw Gateway.app/Contents/MacOS/other',
            'DuDuClaw Gateway.app/Contents/MacOS/subdir/other', expected,
        ]
        for name in invalid:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                self.rewrite_zip(fixture, [(expected, 0o100755, b'gateway'),
                                           (name, 0o100644, b'bad')])
                result = self.run_install(root, fixture)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Unsafe gateway bundle listing', result.stderr)
                self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())
                self.assertFalse((fixture / 'codesign-args').exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_missing_executable_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            self.rewrite_zip(fixture, [('DuDuClaw Gateway.app/Contents/Info.plist',
                                       0o100644, b'fixture')])
            result = self.run_install(root, fixture)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Unsafe gateway bundle listing', result.stderr)

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_extracted_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            self.rewrite_zip(fixture, [
                ('DuDuClaw Gateway.app/Contents/MacOS/duduclaw', 0o100755, b'gateway'),
                ('DuDuClaw Gateway.app/Contents/link', stat.S_IFLNK | 0o777, b'/tmp'),
            ])
            result = self.run_install(root, fixture)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Gateway bundle contains symlinks', result.stderr)
            self.assertFalse((fixture / 'codesign-args').exists())
            self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_failures_preserve_existing_app_and_cli(self):
        for failure in ('signature', 'app', 'link', 'unsigned'):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                result = self.run_install(root, fixture)
                self.assertEqual(result.returncode, 0, result.stderr)
                link = root / 'cli/bin/duduclaw'
                link.write_bytes(b'old binary')
                result = self.run_install(root, fixture,
                    signature_failure=failure == 'signature',
                    move_failure=failure if failure in ('app', 'link') else '',
                    real_codesign=failure == 'unsigned')
                self.assertNotEqual(result.returncode, 0)
                self.assertTrue(link.is_symlink())
                self.assertEqual(link.read_bytes(), b'old binary')
                self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))
                self.assertFalse(list((root / 'cli/bin').glob('.duduclaw-*')))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_cli_failure_rolls_back_first_install(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            result = self.run_install(root, fixture, move_failure='link')
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())
            self.assertFalse((root / 'cli/bin/duduclaw').is_symlink())
            self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))


if __name__ == '__main__':
    unittest.main()
