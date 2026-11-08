"""Keep release signing recoverable without rebuilding, using offline cosign stubs."""

import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]


class ReleaseSigningTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.workflow = yaml.load((ROOT / '.github/workflows/release.yml').read_text(),
                                 Loader=yaml.BaseLoader)
        cls.jobs = cls.workflow['jobs']
        cls.sign_step = next(step for step in cls.jobs['sign']['steps']
                             if step.get('name') == 'Sign and verify archive with retry')

    def action(self, job, action):
        return next(step for step in self.jobs[job]['steps']
                    if step.get('uses', '').startswith(f'actions/{action}@'))

    def test_build_preserves_unsigned_outputs_without_cosign_or_oidc(self):
        build = self.jobs['build']
        self.assertNotIn('cosign', json.dumps(build))
        self.assertNotIn('id-token', build['permissions'])
        upload = self.action('build', 'upload-artifact')
        self.assertEqual(upload['with']['name'], 'unsigned-${{ matrix.artifact }}')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertEqual(upload['with']['path'].splitlines(), [
            'duduclaw-*.tar.gz', 'duduclaw-*.zip', 'duduclaw-*.sha256'])
        # Packaging is the last work on either OS before saving the build outputs.
        self.assertEqual(build['steps'][-1], upload)
        self.assertEqual([s['name'] for s in build['steps'][-3:-1]],
                         ['Package (Unix)', 'Package (Windows)'])

    def test_sign_matrix_keeps_artifact_contract_and_publish_dependencies(self):
        sign = self.jobs['sign']
        self.assertEqual(sign['needs'], 'build')
        self.assertEqual(sign['permissions'], {'contents': 'read', 'id-token': 'write'})
        self.assertEqual(sign['strategy']['fail-fast'], 'false')
        builds = self.jobs['build']['strategy']['matrix']['include']
        self.assertEqual(sign['strategy']['matrix']['include'], [
            {'artifact': row['artifact'],
             'extension': 'zip' if row['os'] == 'windows-latest' else 'tar.gz'}
            for row in builds])
        self.assertEqual(self.action('sign', 'download-artifact')['with']['name'],
                         'unsigned-${{ matrix.artifact }}')
        upload = self.action('sign', 'upload-artifact')
        self.assertEqual(upload['with']['name'], '${{ matrix.artifact }}')
        self.assertEqual(upload['with']['overwrite'], 'true')
        self.assertEqual(upload['with']['if-no-files-found'], 'error')
        self.assertEqual(upload['with']['path'].splitlines(), [
            'duduclaw-*.tar.gz', 'duduclaw-*.zip', 'duduclaw-*.sha256',
            'duduclaw-*.sig', 'duduclaw-*.pem'])
        self.assertGreater(sign['steps'].index(upload), sign['steps'].index(self.sign_step))
        self.assertEqual(self.jobs['release']['needs'], ['sign', 'delivery-gate'])
        for job in ('release', 'npm-publish'):
            download = self.action(job, 'download-artifact')['with']
            self.assertEqual(download['pattern'], 'duduclaw-*')
            self.assertEqual(download['merge-multiple'], 'true')
        self.assertEqual(self.action('npm-publish', 'download-artifact')['with']['path'],
                         'artifacts')
        for job in ('npm-publish', 'pypi-publish'):
            self.assertIn('release', self.jobs[job]['needs'])
        self.assertEqual(self.jobs['pypi-publish']['permissions']['id-token'], 'write')
        self.assertNotIn('id-token', self.workflow['permissions'])

    def run_signing(self, sign_failures=0, verify_failures=0, checksum_failure=0):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            stubs = {
                'cosign': '''#!/bin/bash
set -eu
command=$1
count_file="$PWD/$command.count"
count=0
if [ -f "$count_file" ]; then count=$(cat "$count_file"); fi
count=$((count + 1))
echo "$count" > "$count_file"
printf '%s\\n' "$*" >> calls
case "$command" in
  sign-blob) failures=$SIGN_FAILURES ;;
  verify-blob) failures=$VERIFY_FAILURES ;;
  *) exit 99 ;;
esac
[ "$count" -gt "$failures" ]
''',
                'sleep': '#!/bin/bash\necho "$1" >> delays\n',
                'sha256sum': '#!/bin/bash\nexit "$CHECKSUM_FAILURE"\n',
            }
            for name, source in stubs.items():
                path = root / name
                path.write_text(source)
                path.chmod(0o755)
            result = subprocess.run(['bash', '-c', self.sign_step['run']], cwd=root,
                                    env={**os.environ, 'PATH': f'{root}:{os.environ["PATH"]}',
                                         'ARCHIVE': 'duduclaw-windows-x64.zip',
                                         'CERTIFICATE_IDENTITY': 'https://github.com/repo/workflow@ref',
                                         'SIGN_FAILURES': str(sign_failures),
                                         'VERIFY_FAILURES': str(verify_failures),
                                         'CHECKSUM_FAILURE': str(checksum_failure)},
                                    text=True, capture_output=True, timeout=10)
            def lines(name):
                path = root / name
                return path.read_text().splitlines() if path.exists() else []
            return result, lines('calls'), lines('delays')

    def test_transient_sign_and_verify_errors_retry_independently(self):
        result, calls, delays = self.run_signing(sign_failures=2, verify_failures=1)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(delays, ['30', '60', '30'])
        self.assertEqual(sum(c.startswith('sign-blob ') for c in calls), 3)
        self.assertEqual(sum(c.startswith('verify-blob ') for c in calls), 2)
        self.assertIn('--yes --output-signature duduclaw-windows-x64.zip.sig '
                      '--output-certificate duduclaw-windows-x64.zip.pem', calls[0])
        self.assertIn('--certificate-identity https://github.com/repo/workflow@ref', calls[-1])
        self.assertIn('--certificate-oidc-issuer https://token.actions.githubusercontent.com',
                      calls[-1])
        self.assertIn('--signature duduclaw-windows-x64.zip.sig', calls[-1])
        self.assertIn('--certificate duduclaw-windows-x64.zip.pem', calls[-1])
        self.assertEqual(self.sign_step['env']['CERTIFICATE_IDENTITY'],
                         'https://github.com/${{ github.workflow_ref }}')

    def test_exhausted_sign_or_verify_fails_closed_with_bounded_backoff(self):
        for sign_failures, verify_failures, expected_calls in ((6, 0, 6), (0, 6, 7)):
            with self.subTest(sign_failures=sign_failures):
                result, calls, delays = self.run_signing(sign_failures, verify_failures)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(len(calls), expected_calls)
                self.assertEqual(delays, ['30', '60', '120', '240', '480'])
                self.assertIn('failed after 6 attempts', result.stdout)

    def test_checksum_failure_prevents_signing(self):
        result, calls, delays = self.run_signing(checksum_failure=1)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(calls, [])
        self.assertEqual(delays, [])


if __name__ == '__main__':
    unittest.main()
