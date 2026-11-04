"""Exercise real readelf-shaped input and the binary inspection entry point."""

from pathlib import Path
import subprocess
import sys
import unittest

import yaml
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check_glibc


class GlibcBaselineTests(unittest.TestCase):
    def test_supported_versions_and_numeric_order(self):
        sample = '''Version needs section '.gnu.version_r' contains 1 entry:
          Name: GLIBC_2.9 Flags: none Version: 2
          Name: GLIBC_2.18 Flags: none Version: 3
          Name: GLIBC_2.31 Flags: none Version: 4
          Name: OPENSSL_3.0.0 Flags: none Version: 5'''
        self.assertEqual(check_glibc.check_versions(sample), (2, 31, 0))

    def test_reject_newer_and_missing_symbols(self):
        for sample in ['GLIBC_2.38 GLIBC_2.39', 'GLIBC_2.32', 'GLIBC_2.31.1',
                       'GLIBC_3.0', 'GLIBC_PRIVATE', 'not an ELF']:
            with self.subTest(sample=sample), self.assertRaises(ValueError):
                check_glibc.check_versions(sample)

    def test_linux_release_builds_and_guard_stay_together(self):
        root = Path(__file__).resolve().parents[2]
        for workflow, job_name in [('release.yml', 'build'), ('release-cache-warm.yml', 'release')]:
            document = yaml.safe_load((root / '.github/workflows' / workflow).read_text())
            steps = document['jobs'][job_name]['steps']
            build = next(step for step in steps if step.get('run', '').startswith('cross build'))
            self.assertEqual(build['if'], "runner.os == 'Linux'")
            self.assertIn('duduclaw-gateway/vendored-openssl', build['run'])
            guard = next(step for step in steps if step.get('name') == 'Check Linux glibc baseline')
            self.assertEqual(guard['if'], "runner.os == 'Linux'")
            self.assertIn('scripts/check_glibc.py', guard['run'])
            self.assertGreater(steps.index(guard), steps.index(build))
            if workflow == 'release.yml':
                package = next(step for step in steps if step.get('name') == 'Package (Unix)')
                self.assertLess(steps.index(guard), steps.index(package))

    def test_sidecar_guard_runs_before_staging(self):
        root = Path(__file__).resolve().parents[2]
        document = yaml.safe_load((root / '.github/workflows/desktop-release.yml').read_text())
        step = next(step for step in document['jobs']['build']['steps']
                    if step.get('name') == 'Build & stage gateway sidecar')
        self.assertIn('SIDECAR_TARGET_DIR=target/linux-sidecar', step['run'])
        self.assertIn('CARGO_TARGET_DIR="$SIDECAR_TARGET_DIR"', step['run'])
        self.assertLess(step['run'].index('scripts/check_glibc.py'), step['run'].index('cp '))

    def test_command_and_exit_status(self):
        with patch.object(sys, 'argv', ['check_glibc.py', 'binary']), \
             patch.object(subprocess, 'run') as run:
            for sample, status in [('Name: GLIBC_2.31', 0), ('Name: GLIBC_2.39', 1), ('', 1)]:
                run.return_value = subprocess.CompletedProcess([], 0, sample, '')
                self.assertEqual(check_glibc.main(), status)
            run.assert_called_with(['readelf', '--version-info', '--', 'binary'],
                                   check=True, capture_output=True, text=True)
            run.side_effect = subprocess.CalledProcessError(1, 'readelf')
            self.assertEqual(check_glibc.main(), 1)
            run.side_effect = FileNotFoundError('readelf')
            self.assertEqual(check_glibc.main(), 1)


if __name__ == '__main__':
    unittest.main()
