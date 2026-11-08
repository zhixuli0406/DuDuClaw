"""Exercise the release installer with local fixtures and no network or builds."""
import hashlib
import io
import os
from pathlib import Path
import re
import shutil
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
PINNED_KEY = re.search(r'^RELEASE_PUBKEY="([^"]+)"$', INSTALLER, re.M)[1]
GATEWAY_DR = re.search(r"^GATEWAY_DR='([^']+)'$", INSTALLER, re.M)[1]
EXE = 'DuDuClaw Gateway.app/Contents/MacOS/duduclaw'
PLIST = 'DuDuClaw Gateway.app/Contents/Info.plist'
VERSION = '1.81.0'


def payload(version=VERSION):
    return f"#!/bin/sh\nprintf 'duduclaw {version}\\n'\n".encode()


def _reg(name, mode=0o644):
    entry = zipfile.ZipInfo(name)
    entry.create_system = 3
    entry.external_attr = (0o100000 | mode) << 16
    return entry


def info_plist(identifier='ai.duduclaw.gateway', version=VERSION, executable='duduclaw'):
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        '<plist version="1.0"><dict>'
        f'<key>CFBundleIdentifier</key><string>{identifier}</string>'
        f'<key>CFBundleShortVersionString</key><string>{version}</string>'
        f'<key>CFBundleExecutable</key><string>{executable}</string>'
        '</dict></plist>\n').encode()


def minisig(comment, algorithm='RUQ'):
    return (f'untrusted comment: signature from minisign secret key\n{algorithm}AAAAAAAAAAAAAAAA=\n'
            f'trusted comment: {comment}\nAAAAAAAAAAAAAAAAAAAA==\n')


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
    *.minisig) cp "$FIXTURE/$ARCHIVE.minisig" "$2" ;;
    *.tar.gz) cp "$FIXTURE/bare.tar.gz" "$2" ;;
    *) return 1 ;;
  esac
}
has_cmd() {
  if [ "$1" = minisign ] && [ "$MINISIGN_MODE" = absent ]; then return 1; fi
  command -v "$1" >/dev/null 2>&1
}
if [ "$MINISIGN_MODE" != real ]; then
  minisign() {
    printf '%s\n' "$@" > "$FIXTURE/minisign-args"
    [ "$MINISIGN_MODE" != bad ]
  }
fi
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

TARGETS = ('duduclaw-darwin-arm64', 'duduclaw-linux-x64')


class InstallTests(unittest.TestCase):
    def write_sidecars(self, fixture, bad_checksum=False):
        for ext, file in (('app.zip', 'app.zip'), ('tar.gz', 'bare.tar.gz')):
            digest = '0' * 64 if bad_checksum else hashlib.sha256((fixture / file).read_bytes()).hexdigest()
            for target in TARGETS:
                name = f'{target}.{ext}'
                (fixture / f'{name}.sha256').write_text(f'{digest}  {name}\n')
                (fixture / f'{name}.minisig').write_text(minisig(f'duduclaw v{VERSION} {name}'))

    def fixtures(self, root, bad_checksum=False):
        fixture = root / 'fixtures'
        fixture.mkdir()
        with zipfile.ZipFile(fixture / 'app.zip', 'w') as z:
            z.writestr(_reg(EXE, 0o755), payload())
            z.writestr(_reg(PLIST), info_plist())
        with tarfile.open(fixture / 'bare.tar.gz', 'w:gz') as tar:
            entry = tarfile.TarInfo('duduclaw')
            entry.size = len(payload())
            entry.mode = 0o755
            tar.addfile(entry, io.BytesIO(payload()))
        self.write_sidecars(fixture, bad_checksum)
        return fixture

    def run_install(self, root, fixture, status='200', target='duduclaw-darwin-arm64',
                    signature_failure=False, move_failure='', real_codesign=False,
                    minisign_mode='stub', installer=None):
        ext = 'app.zip' if status == '200' and 'darwin' in target else 'tar.gz'
        env = dict(os.environ, HOME=str(root / 'home'), DUDUCLAW_HOME=str(root / 'cli'),
                   DUDUCLAW_APP_DIR=str(root / 'apps'), DUDUCLAW_VERSION=VERSION,
                   FIXTURE=str(fixture), HTTP_STATUS=status, TARGET=target,
                   ARCHIVE=f'{target}.{ext}', SIGNATURE_FAILURE=str(int(signature_failure)),
                   MOVE_FAILURE=move_failure, MINISIGN_MODE=minisign_mode)
        installer = installer or INSTALLER
        if not real_codesign:
            installer = installer.replace('/usr/bin/codesign --verify', 'codesign_fixture --verify')
        return subprocess.run(['bash', '-c', installer + '\n' + STUBS], env=env,
                              capture_output=True, text=True)

    def assert_nothing_installed(self, root):
        self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())
        self.assertFalse((root / 'cli/bin/duduclaw').exists())
        self.assertFalse(list((root / 'apps').glob('.duduclaw-*')) if (root / 'apps').exists() else [])

    # ── release signature / checksum (finding 1) ─────────────────────────

    def test_checksum_mismatch_is_fatal_on_mac_and_linux(self):
        for target in TARGETS:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root, bad_checksum=True)
                result = self.run_install(root, fixture, target=target)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Checksum verification failed', result.stderr)
                self.assert_nothing_installed(root)

    def test_signature_is_verified_with_the_pinned_key_on_mac_and_linux(self):
        for target in TARGETS:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                result = self.run_install(root, fixture, target=target, minisign_mode='bad')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Release signature verification FAILED', result.stderr)
                self.assert_nothing_installed(root)
                args = (fixture / 'minisign-args').read_text().splitlines()
                self.assertEqual(args[0], '-V')
                self.assertEqual(args[args.index('-P') + 1], PINNED_KEY)
                ext = 'app.zip' if 'darwin' in target else 'tar.gz'
                self.assertTrue(args[args.index('-m') + 1].endswith(f'/{target}.{ext}'))
                self.assertTrue(args[args.index('-x') + 1].endswith(f'/{target}.{ext}.minisig'))

    def test_missing_minisign_refuses_instead_of_skipping(self):
        for target in TARGETS:
            with self.subTest(target=target), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                result = self.run_install(root, fixture, target=target, minisign_mode='absent')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('minisign is required', result.stderr)
                self.assert_nothing_installed(root)

    def test_missing_signature_or_checksum_is_fatal(self):
        for sidecar in ('minisig', 'sha256'):
            with self.subTest(sidecar=sidecar), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                (fixture / f'duduclaw-linux-x64.tar.gz.{sidecar}').unlink()
                result = self.run_install(root, fixture, target='duduclaw-linux-x64')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('unavailable', result.stderr)
                self.assert_nothing_installed(root)

    def test_signed_comment_must_name_this_version_and_file(self):
        name = 'duduclaw-linux-x64.tar.gz'
        bad = [
            minisig(f'duduclaw v1.80.0 {name}'),
            minisig(f'duduclaw v{VERSION} duduclaw-linux-arm64.tar.gz'),
            minisig(f'duduclaw-pro v{VERSION} {name}'),
            minisig(f'duduclaw v{VERSION} {name}', algorithm='RWQ'),  # legacy, non-prehashed
            minisig(f'duduclaw v{VERSION} {name}') + 'extra line\n',
            minisig(f'duduclaw v{VERSION} {name} '),
        ]
        for text in bad:
            with self.subTest(text=text), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                (fixture / f'{name}.minisig').write_text(text)
                result = self.run_install(root, fixture, target='duduclaw-linux-x64')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('malformed or names another version/file', result.stderr)
                self.assert_nothing_installed(root)

    def test_checksum_sidecar_is_bound_to_the_exact_file_name(self):
        name = 'duduclaw-linux-x64.tar.gz'
        for layout in ('other-name', 'two-lines', 'no-name', 'uppercase'):
            with self.subTest(layout=layout), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                digest = hashlib.sha256((fixture / 'bare.tar.gz').read_bytes()).hexdigest()
                text = {
                    'other-name': f'{digest}  duduclaw-linux-arm64.tar.gz\n',
                    'two-lines': f'{"0" * 64}  {name}\n{digest}  {name}\n',
                    'no-name': f'{digest}\n',
                    'uppercase': f'{digest.upper()}  {name}\n',
                }[layout]
                (fixture / f'{name}.sha256').write_text(text)
                result = self.run_install(root, fixture, target='duduclaw-linux-x64')
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('Checksum verification failed', result.stderr)
                self.assert_nothing_installed(root)

    @unittest.skipUnless(shutil.which('minisign'), 'minisign not installed')
    def test_real_minisign_signature_round_trip(self):
        name = 'duduclaw-linux-x64.tar.gz'
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            keys = root / 'keys'
            keys.mkdir()
            subprocess.run(['minisign', '-G', '-W', '-p', str(keys / 'pub'), '-s', str(keys / 'sec')],
                           check=True, capture_output=True)
            test_key = (keys / 'pub').read_text().splitlines()[1]
            installer = INSTALLER.replace(PINNED_KEY, test_key)

            def sign(path, comment):
                subprocess.run(['minisign', '-S', '-s', str(keys / 'sec'), '-m', str(path),
                                '-t', comment, '-x', str(fixture / f'{name}.minisig')],
                               check=True, capture_output=True)

            # Signed over different bytes: the payload signature fails.
            other = root / 'other'
            other.write_bytes(b'not the archive')
            sign(other, f'duduclaw v{VERSION} {name}')
            result = self.run_install(root, fixture, target='duduclaw-linux-x64',
                                      minisign_mode='real', installer=installer)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Release signature verification FAILED', result.stderr)
            self.assert_nothing_installed(root)
            # A correct signature under a different key (the production key) fails.
            sign(fixture / 'bare.tar.gz', f'duduclaw v{VERSION} {name}')
            result = self.run_install(root, fixture, target='duduclaw-linux-x64', minisign_mode='real')
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('Release signature verification FAILED', result.stderr)
            # The right key and comment install.
            result = self.run_install(root, fixture, target='duduclaw-linux-x64',
                                      minisign_mode='real', installer=installer)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((root / 'cli/bin/duduclaw').read_bytes(), payload())

    def test_installer_pins_match_the_rust_updater(self):
        publishers = (ROOT / 'crates/duduclaw-core/src/publishers.rs').read_text()
        key = re.search(r'OFFICIAL_PUBLISHER_KEY: &str = "([^"]+)"', publishers)[1]
        self.assertEqual(PINNED_KEY, key)
        core = (ROOT / 'crates/duduclaw-core/src/macos_bundle.rs').read_text()
        block = re.search(r'pub const PINNED_DR: &str = concat!\((.*?)\);', core, re.S)[1]
        literals = re.findall(r'"((?:[^"\\]|\\.)*)"', block)
        self.assertEqual(GATEWAY_DR, ''.join(l.replace('\\"', '"') for l in literals))

    # ── install / replacement ────────────────────────────────────────────

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
                self.assertEqual(link.read_bytes(), payload())
                self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))
                # The pinned designated requirement, not just "some valid signature".
                args = (fixture / 'codesign-args').read_text().splitlines()
                self.assertEqual(args[:4], ['--verify', '--strict', '--deep', '-R'])
                self.assertEqual(args[4], '=' + GATEWAY_DR)
                self.assertIn('.duduclaw-install.', args[5])
                link.write_bytes(b'old binary')

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_bundle_helper_runs_in_posix_shell_without_python(self):
        helper = INSTALLER.split('verify_release_archive() (', 1)[1].split(
            '# ---------------------------------------------------------------------------', 1)[0]
        helper = 'verify_release_archive() (' + helper
        helper = helper.replace('/usr/bin/codesign --verify', 'codesign_fixture --verify')
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            name = 'duduclaw-darwin-arm64.app.zip'
            script = f"""set -eu
RELEASE_PUBKEY='{PINNED_KEY}'
GATEWAY_DR='{GATEWAY_DR}'
fatal() {{ echo "$*" >&2; exit 1; }}
has_cmd() {{ command -v "$1" >/dev/null 2>&1; }}
python3() {{ exit 99; }}
minisign() {{ [ "$1" = -V ]; }}
codesign_fixture() {{ [ "$1" = --verify ] && [ "$2" = --strict ]; }}
""" + helper + '\nverify_release_archive "$1" "$5" "$6" "$1.sha256" "$1.minisig"\n' \
                + 'install_macos_bundle "$1" "$2" "$3" "$6"\n'
            archive = root / name
            shutil.copy(fixture / 'app.zip', archive)
            shutil.copy(fixture / f'{name}.sha256', root / f'{name}.sha256')
            shutil.copy(fixture / f'{name}.minisig', root / f'{name}.minisig')
            result = subprocess.run(['/bin/sh', '-c', script, 'fixture',
                str(archive), str(root / 'apps'), str(root / 'cli/bin'), '', name, VERSION],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            link = root / 'cli/bin/duduclaw'
            self.assertTrue(link.is_symlink())
            self.assertEqual(link.read_bytes(), payload())

    def rewrite_zip(self, fixture, entries, create_system=3):
        with zipfile.ZipFile(fixture / 'app.zip', 'w') as archive:
            for name, mode, body in entries:
                entry = zipfile.ZipInfo(name)
                entry.create_system = create_system
                entry.external_attr = mode << 16
                archive.writestr(entry, body)
        digest = hashlib.sha256((fixture / 'app.zip').read_bytes()).hexdigest()
        (fixture / 'duduclaw-darwin-arm64.app.zip.sha256').write_text(
            f'{digest}  duduclaw-darwin-arm64.app.zip\n')

    def good_entries(self):
        return [(EXE, 0o100755, payload()), (PLIST, 0o100644, info_plist())]

    def assert_listing_refused(self, root, fixture, result):
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Unsafe gateway bundle listing', result.stderr)
        self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())
        self.assertFalse((fixture / 'codesign-args').exists())
        self.assertFalse(list((root / 'apps').glob('.duduclaw-*')))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_unsafe_listings_are_rejected_before_extracting(self):
        invalid = [
            '/outside', '../outside', 'DuDuClaw Gateway.app/../outside',
            'Other.app/file', 'DuDuClaw Gateway.app/Contents/MacOS/other',
            'DuDuClaw Gateway.app/Contents/MacOS/subdir/other', EXE,
            'DuDuClaw Gateway.app/contents/info.plist',            # case collision
            'DuDuClaw Gateway.app/Contents/Info.plist/inner',      # file used as dir
            'DuDuClaw Gateway.app/Contents/Resources/a^Jb',        # unusual characters
            'DuDuClaw Gateway.app/Contents/Resources/café',   # non-ASCII (NFC/NFD)
        ]
        for name in invalid:
            with self.subTest(name=name), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                self.rewrite_zip(fixture, self.good_entries() + [(name, 0o100644, b'bad')])
                self.assert_listing_refused(root, fixture, self.run_install(root, fixture))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_unsafe_modes_are_rejected_before_extracting(self):
        resource = 'DuDuClaw Gateway.app/Contents/Resources/'
        cases = {
            'symlink': [(resource + 'link', stat.S_IFLNK | 0o755, b'/tmp')],
            'fifo': [(resource + 'fifo', stat.S_IFIFO | 0o644, b'')],
            'char device': [(resource + 'dev', stat.S_IFCHR | 0o644, b'')],
            'setuid exe': [(EXE, 0o104755, payload())],
            'setgid file': [(resource + 'g', 0o102644, b'x')],
            'sticky dir': [(resource + 'd/', 0o041755, b'')],
            'group-writable file': [(resource + 'w', 0o100664, b'x')],
            'world-writable file': [(resource + 'w', 0o100646, b'x')],
            'world-writable dir': [(resource + 'd/', 0o040777, b'')],
            'extra executable': [(resource + 'run', 0o100755, b'x')],
            'no type bits': [(resource + 'raw', 0o000644, b'x')],
        }
        for what, extra in cases.items():
            with self.subTest(what=what), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                entries = [e for e in self.good_entries() if e[0] not in {x[0] for x in extra}]
                self.rewrite_zip(fixture, entries + extra)
                self.assert_listing_refused(root, fixture, self.run_install(root, fixture))
        # Entries not made on a unix host carry no trustworthy mode.
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            self.rewrite_zip(fixture, self.good_entries(), create_system=0)
            self.assert_listing_refused(root, fixture, self.run_install(root, fixture))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_missing_executable_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            fixture = self.fixtures(root)
            self.rewrite_zip(fixture, [(PLIST, 0o100644, info_plist())])
            self.assert_listing_refused(root, fixture, self.run_install(root, fixture))

    @unittest.skipUnless(sys.platform == 'darwin', 'macOS built-in bundle tools required')
    def test_bundle_identity_must_match_the_signed_release(self):
        cases = {
            'identifier': info_plist(identifier='ai.duduclaw.desktop'),
            'version': info_plist(version='1.80.0'),
            'edition': info_plist(executable='duduclaw-pro'),
            'binary version': None,
        }
        for what, plist in cases.items():
            with self.subTest(what=what), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                fixture = self.fixtures(root)
                exe_body = payload('1.80.0') if plist is None else payload()
                self.rewrite_zip(fixture, [(EXE, 0o100755, exe_body),
                                           (PLIST, 0o100644, plist or info_plist())])
                result = self.run_install(root, fixture)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('mismatch', result.stderr)
                self.assertTrue((fixture / 'codesign-args').exists(), 'signature checked first')
                self.assertFalse((root / 'apps/DuDuClaw Gateway.app').exists())
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
