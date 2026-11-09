import importlib.util
import hashlib
import os
import stat
from pathlib import Path
import subprocess
import tempfile
import unittest
from contextlib import contextmanager
from types import SimpleNamespace
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('runtime_tree', Path(__file__).with_name('build-runtime-tree.py'))
RUNTIME = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(RUNTIME)


class RuntimeTreeTests(unittest.TestCase):
    def test_closure_contains_only_resolved_library_files(self):
        output = '''
linux-vdso.so.1 (0x1234)
libc.so.6 => /lib/x86_64-linux-gnu/libc.so.6 (0x1234)
/lib64/ld-linux-x86-64.so.2 (0x5678)
'''
        self.assertEqual(RUNTIME.dependency_paths(output), {
            '/lib/x86_64-linux-gnu/libc.so.6', '/lib64/ld-linux-x86-64.so.2'})

    def test_unresolved_or_unrelated_dependencies_are_refused(self):
        for output in ['libsecret.so => not found', 'unexpected loader output',
                       '/usr/local/lib/secret.so (0x12)',
                       '/usr/lib/duduclaw/other-component.so (0x12)',
                       '/lib/../usr/local/secret.so (0x12)',
                       '/home/employee/library.so (0x12)']:
            with self.subTest(output=output), self.assertRaises(ValueError):
                RUNTIME.dependency_paths(output)

    def test_loader_is_fixed_and_environment_is_clean(self):
        outputs = [subprocess.CompletedProcess([], 0,
            '[Requesting program interpreter: /lib64/ld-linux-x86-64.so.2]'),
            subprocess.CompletedProcess([], 0, 'libc.so.6 => /lib/libc.so.6 (0x1234)')]
        with patch.object(RUNTIME, 'trusted_file'), patch.object(RUNTIME.subprocess, 'run', side_effect=outputs) as run:
            self.assertEqual(RUNTIME.inspect_closure(Path('/signed/worker')),
                             {'/lib64/ld-linux-x86-64.so.2', '/lib/libc.so.6'})
        self.assertEqual(run.call_args.args[0], ['/lib64/ld-linux-x86-64.so.2', '--list', '/signed/worker'])
        self.assertEqual(run.call_args.kwargs['env'], {'PATH': '/usr/bin:/bin', 'LC_ALL': 'C'})
        with patch.object(RUNTIME, 'trusted_file'), patch.object(RUNTIME.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0,
                '[Requesting program interpreter: /tmp/untrusted-loader]')):
            with self.assertRaises(ValueError):
                RUNTIME.inspect_closure(Path('/signed/worker'))

    def test_digest_checked_before_inspecting_or_creating_tree(self):
        with tempfile.TemporaryDirectory() as temporary:
            worker = Path(temporary) / 'worker'
            worker.write_bytes(b'\x7fELFfake')
            destination = Path(temporary) / 'runtime'
            with patch.object(RUNTIME, 'trusted_file', return_value=worker), patch.object(RUNTIME, 'inspect_closure') as inspect:
                with self.assertRaises(ValueError):
                    RUNTIME.build_tree(worker, '0' * 64, destination)
                inspect.assert_not_called()
                self.assertFalse(destination.exists())

    def test_untrusted_inspection_tool_never_executes(self):
        with patch.object(RUNTIME, 'trusted_file', side_effect=ValueError('untrusted tool')), \
                patch.object(RUNTIME.subprocess, 'run') as run:
            with self.assertRaises(ValueError):
                RUNTIME.inspect_closure(Path('/signed/worker'))
            run.assert_not_called()

    def test_build_copies_only_worker_and_exact_dependency_files(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            worker = base / 'worker'
            worker.write_bytes(b'\x7fELFfake')
            library = base / 'libc.so.6'
            library.write_bytes(b'library')
            (base / 'unrelated-secret').write_bytes(b'not projected')
            destination = base / 'runtime'
            digest = hashlib.sha256(worker.read_bytes()).hexdigest()
            def trusted(path):
                return worker if Path(path) == worker else library
            with patch.object(RUNTIME, 'trusted_file', side_effect=trusted), \
                    patch.object(RUNTIME, 'inspect_closure', return_value={'/lib/libc.so.6'}), \
                    patch.object(RUNTIME, 'protected_destination'):
                old_umask = os.umask(0o077)
                try:
                    RUNTIME.build_tree(worker, digest, destination)
                finally:
                    os.umask(old_umask)
                with self.assertRaises(FileExistsError):
                    RUNTIME.build_tree(worker, digest, destination)
            files = {str(path.relative_to(destination)) for path in destination.rglob('*') if path.is_file()}
            self.assertEqual(files, {'usr/libexec/duduclaw/duduclaw-parse-worker', 'lib/libc.so.6'})
            for directory in [destination, *[p for p in destination.rglob('*') if p.is_dir()]]:
                self.assertEqual(stat.S_IMODE(directory.stat().st_mode), 0o755)
            self.assertFalse((destination / 'usr/local').exists())
            self.assertFalse((destination / 'usr/lib/duduclaw').exists())

    @contextmanager
    def installed_fixture(self):
        with tempfile.TemporaryDirectory() as temporary:
            base = Path(temporary)
            worker = base / 'worker'
            worker.write_bytes(b'\x7fELFfixture')
            digest = hashlib.sha256(worker.read_bytes()).hexdigest()
            destination = base / 'runtime'
            with patch.object(RUNTIME, 'trusted_file', return_value=worker), \
                    patch.object(RUNTIME, 'inspect_closure', return_value=set()), \
                    patch.object(RUNTIME, 'protected_destination'), \
                    patch.object(RUNTIME, 'protected_tree_entry', side_effect=lambda info:
                        info.st_uid == os.getuid() and info.st_gid == os.getgid()
                        and stat.S_IMODE(info.st_mode) == 0o755):
                yield worker, digest, destination

    def test_install_publishes_fresh_tree_and_readback_is_idempotent(self):
        with self.installed_fixture() as (worker, digest, destination):
            RUNTIME.install_tree(worker, digest, destination)
            identity = destination.stat().st_ino
            files = RUNTIME.verify_tree(worker, digest, destination)
            self.assertEqual(files, [str(RUNTIME.WORKER_TARGET)])
            RUNTIME.install_tree(worker, digest, destination)
            self.assertEqual(destination.stat().st_ino, identity)
            self.assertEqual(list(destination.parent.glob('.parse-runtime-*')), [])

    def test_install_replaces_only_empty_legacy_tree(self):
        with self.installed_fixture() as (worker, digest, destination):
            destination.mkdir(mode=0o755)
            destination.chmod(0o755)
            RUNTIME.install_tree(worker, digest, destination)
            self.assertTrue(destination.joinpath(*RUNTIME.WORKER_TARGET.parts[1:]).is_file())
            unknown = destination / 'unrelated'
            unknown.write_bytes(b'preserve')
            unknown.chmod(0o755)
            with self.assertRaises(ValueError):
                RUNTIME.install_tree(worker, digest, destination)
            self.assertEqual(unknown.read_bytes(), b'preserve')

    def test_existing_tree_refuses_changed_bytes_links_modes_and_missing_worker(self):
        for mutation in ['bytes', 'symlink', 'hardlink', 'mode', 'missing', 'directory']:
            with self.subTest(mutation=mutation), self.installed_fixture() as (worker, digest, destination):
                RUNTIME.install_tree(worker, digest, destination)
                copied = destination.joinpath(*RUNTIME.WORKER_TARGET.parts[1:])
                if mutation == 'bytes':
                    copied.write_bytes(b'changed')
                elif mutation == 'symlink':
                    copied.unlink()
                    copied.symlink_to(worker)
                elif mutation == 'hardlink':
                    os.link(copied, destination.parent / 'alias')
                elif mutation == 'mode':
                    copied.chmod(0o777)
                elif mutation == 'missing':
                    copied.unlink()
                elif mutation == 'directory':
                    (destination / 'extra').mkdir(mode=0o755)
                with self.assertRaises(ValueError):
                    RUNTIME.verify_tree(worker, digest, destination)

    def test_failed_publication_preserves_old_tree_and_inactive_stage(self):
        with self.installed_fixture() as (worker, digest, destination):
            destination.mkdir(mode=0o755)
            destination.chmod(0o755)
            identity = destination.stat().st_ino
            with patch.object(RUNTIME.os, 'replace', side_effect=OSError('publish failed')):
                with self.assertRaises(OSError):
                    RUNTIME.install_tree(worker, digest, destination)
            self.assertEqual(destination.stat().st_ino, identity)
            self.assertEqual(list(destination.iterdir()), [])
            self.assertEqual(len(list(destination.parent.glob('.parse-runtime-*'))), 1)

    def test_tree_entries_require_root_ownership_and_exact_mode(self):
        self.assertTrue(RUNTIME.protected_tree_entry(SimpleNamespace(st_uid=0, st_gid=0, st_mode=0o100755)))
        for uid, gid, mode in [(1, 0, 0o755), (0, 1, 0o755), (0, 0, 0o775), (0, 0, 0o4755)]:
            self.assertFalse(RUNTIME.protected_tree_entry(SimpleNamespace(st_uid=uid, st_gid=gid, st_mode=mode)))

    def test_service_never_projects_host_usr_or_lib(self):
        service = Path(__file__).parents[1] / 'systemd/duduclaw-parse@.service'
        binds = [line for line in service.read_text().splitlines() if line.startswith('BindReadOnlyPaths=')]
        self.assertEqual(binds, ['BindReadOnlyPaths=/usr/lib/duduclaw/parse-root-tree:/:norbind'])
        self.assertIn('InaccessiblePaths=-/usr/local -/usr/lib/duduclaw', service.read_text())


if __name__ == '__main__':
    unittest.main()
