"""Offline bootstrap tests. Never invokes a real gcloud installation."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name('bootstrap.sh')


class BootstrapTests(unittest.TestCase):
    def run_script(self, *args, env=None):
        return subprocess.run(['bash', str(SCRIPT), *args], text=True, capture_output=True, env=env)

    def test_http_dry_run(self):
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / 'never-created'
            result = self.run_script('--dry-run', '--project-id', 'demo-chat-123', '--endpoint-url',
                                     'https://chat.example.com/webhook/googlechat', '--out', str(out))
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn('services enable chat.googleapis.com', result.stdout)
            self.assertIn('Authentication Audience: Project number', result.stdout)
            self.assertFalse(out.exists())

    def test_http_agent_dry_run_preserves_literal_name(self):
        result = self.run_script('--dry-run', '--project-id', 'demo-chat-123', '--endpoint-url',
                                 'https://chat.example.com/webhook/googlechat/agents/alice',
                                 '--agent', 'alice', '--app-name', 'A $(touch /bad)')
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--agent alice', result.stdout)
        self.assertIn('App name: A $(touch /bad)', result.stdout)
        self.assertIn('/webhook/googlechat/agents/alice', result.stdout)

    def test_app_name_limit_and_control_characters(self):
        for extra in [['--app-name', 'x' * 26], ['--agent', 'a' * 64]]:
            result = self.run_script('--dry-run', '--endpoint-url', 'https://chat.example.com/webhook/googlechat', *extra)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('25', result.stderr)
        for flag in ['--app-name', '--out']:
            for control in ['\n', '\r', '\t', '\x01', '\x7f', '\x85', '\x9f']:
                with self.subTest(flag=flag, control=repr(control)):
                    result = self.run_script('--dry-run', '--endpoint-url', 'https://chat.example.com/webhook/googlechat', flag, 'x' + control + 'y')
                    self.assertNotEqual(result.returncode, 0)
        result = self.run_script('--dry-run', '--endpoint-url', 'https://chat.example.com/webhook/googlechat', '--app-name', '測' * 25)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('Build this Chat app as a Google Workspace add-on', result.stdout)

    def test_invalid_arguments_fail_before_gcloud(self):
        for args in [[], ['--project-id'], ['--oops'], ['--connection', 'push'],
                     ['--endpoint-url', 'https://chat.example.com/webhook/googlechat', '--project-id', '../evil'],
                     ['--endpoint-url', 'http://example.com'],
                     ['--endpoint-url', 'https://user:pass@example.com'],
                     ['--endpoint-url', 'https://example.com#fragment'],
                     ['--endpoint-url', 'https://example.com:bad'],
                     ['--endpoint-url', 'https://chat.example.com/webhook/googlechat', '--agent', '../alice']]:
            with self.subTest(args=args):
                result = self.run_script('--dry-run', *args)
                self.assertNotEqual(result.returncode, 0)

    def fake_environment(self, directory, failure=''):
        fake = Path(directory) / 'bin'
        fake.mkdir()
        executable = fake / 'gcloud'
        executable.write_text('''#!/usr/bin/env python3
import json, os, pathlib, sys
args = sys.argv[1:]
with open(os.environ['GCLOUD_TEST_LOG'], 'a') as log: log.write(json.dumps(args)+'\\n')
if args[:2] == ['auth', 'list']: print('operator@example.com')
elif args[:2] == ['projects', 'describe']:
    if os.environ.get('PROJECT_NEW') == '1' and not pathlib.Path(os.environ['GCLOUD_TEST_LOG']+'.created').exists(): sys.exit(1)
    print('123456789012')
elif args[:2] == ['projects', 'create']: pathlib.Path(os.environ['GCLOUD_TEST_LOG']+'.created').touch()
elif args[:2] == ['projects', 'get-iam-policy']:
    print(json.dumps({'bindings': [{'role': 'roles/owner', 'members': ['user:operator@example.com']}] if os.environ.get('FAIL') != 'owner' else []}))
elif args[:3] == ['iam', 'service-accounts', 'describe']: sys.exit(1)
elif args[:4] == ['iam', 'service-accounts', 'keys', 'create']:
    if os.environ.get('FAIL') == 'key':
        print('FAILED_PRECONDITION: iam.disableServiceAccountKeyCreation', file=sys.stderr); sys.exit(1)
    pathlib.Path(args[4]).write_text(json.dumps({'type': 'service_account', 'project_id': 'demo-chat-123', 'client_email': 'duduclaw-chat@demo-chat-123.iam.gserviceaccount.com', 'private_key': 'fake-test-key', 'private_key_id': 'a'*40}))
    if os.environ.get('FAIL') == 'partial': sys.exit(1)
elif args[:2] == ['services', 'enable'] and os.environ.get('FAIL') == 'billing':
    print('BILLING_DISABLED', file=sys.stderr); sys.exit(1)
''')
        executable.chmod(0o700)
        return dict(os.environ, PATH=str(fake)+os.pathsep+os.environ['PATH'],
                    GCLOUD_TEST_LOG=str(Path(directory)/'commands.jsonl'), FAIL=failure)

    def test_mocked_success_private_files_and_key_reuse(self):
        with tempfile.TemporaryDirectory() as td:
            env = self.fake_environment(td)
            args = ['--project-id', 'demo-chat-123', '--endpoint-url', 'https://chat.example.com/webhook/googlechat', '--out', str(Path(td)/'output')]
            for run in range(2):
                result = self.run_script(*args, env=env)
                self.assertEqual(result.returncode, 0, result.stderr)
                summary = json.loads((Path(td)/'output'/'bootstrap-result.json').read_text())
                self.assertEqual(summary['created_this_run'], run == 0)
                self.assertEqual(summary['key_id'], 'a'*40)
                self.assertEqual(summary['project_id'], 'demo-chat-123')
                self.assertEqual(summary['service_account_email'], 'duduclaw-chat@demo-chat-123.iam.gserviceaccount.com')
                self.assertNotIn('private_key', summary)
                self.assertIn('After script success the key is kept', result.stdout)
            output = Path(td)/'output'
            summary = json.loads((output/'bootstrap-result.json').read_text())
            self.assertEqual(summary['project_number'], '123456789012')
            self.assertEqual(Path(summary['key_file']).stat().st_mode & 0o777, 0o600)
            self.assertEqual((output/'bootstrap-result.json').stat().st_mode & 0o777, 0o600)
            calls = [json.loads(x) for x in Path(env['GCLOUD_TEST_LOG']).read_text().splitlines()]
            self.assertEqual(sum(c[:4] == ['iam', 'service-accounts', 'keys', 'create'] for c in calls), 1)
            create_index = next(i for i, c in enumerate(calls) if c[:4] == ['iam', 'service-accounts', 'keys', 'create'])
            # A second run may verify resources, but the first run creates the key last.
            self.assertEqual(calls[create_index + 1][:2], ['auth', 'list'])

    def test_mocked_policy_billing_and_owner_errors(self):
        for failure, expected in [('key', 'iam.managed.disableServiceAccountKeyCreation'),
                                  ('billing', 'billing/linkedaccount?project=demo-chat-123'),
                                  ('owner', 'direct Owner role')]:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as td:
                env = self.fake_environment(td, failure)
                result = self.run_script('--project-id', 'demo-chat-123', '--endpoint-url', 'https://chat.example.com/webhook/googlechat',
                                         '--out', str(Path(td)/'output'), env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(expected, result.stderr)
                self.assertFalse((Path(td)/'output'/'bootstrap-result.json').exists())

    def test_failure_after_creation_revokes_cloud_key_and_removes_local_files(self):
        for failure in ['result', 'install', 'chmod', 'partial']:
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as td:
                env = self.fake_environment(td, failure)
                out = Path(td)/'output'
                if failure == 'result':
                    out.mkdir(mode=0o700)
                    (out/'bootstrap-result.json').mkdir()
                if failure in ['install', 'chmod']:
                    command = Path(td)/'bin'/('ln' if failure == 'install' else 'chmod')
                    command.write_text('#!/bin/sh\nexit 1\n')
                    command.chmod(0o700)
                result = self.run_script('--project-id', 'demo-chat-123', '--endpoint-url',
                                         'https://chat.example.com/webhook/googlechat', '--out', str(out), env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertFalse((out/'service-account.json').exists())
                self.assertFalse(list(out.glob('.bootstrap.*')))
                calls = [json.loads(x) for x in Path(env['GCLOUD_TEST_LOG']).read_text().splitlines()]
                deletes = [c for c in calls if c[:4] == ['iam', 'service-accounts', 'keys', 'delete']]
                self.assertEqual(len(deletes), 1)
                self.assertEqual(deletes[0][4], 'a'*40)
                self.assertIn('--iam-account=duduclaw-chat@demo-chat-123.iam.gserviceaccount.com', deletes[0])

    def test_key_checks_ignore_python_optimization_and_reject_links(self):
        for unsafe in ['permissions', 'mismatch', 'symlink', 'hardlink']:
            with self.subTest(unsafe=unsafe), tempfile.TemporaryDirectory() as td:
                env = self.fake_environment(td)
                env['PYTHONOPTIMIZE'] = '1'
                out = Path(td)/'output'
                out.mkdir(mode=0o700)
                key = out/'service-account.json'
                key.write_text(json.dumps({'type': 'service_account', 'project_id': 'wrong' if unsafe == 'mismatch' else 'demo-chat-123', 'client_email': 'duduclaw-chat@demo-chat-123.iam.gserviceaccount.com', 'private_key': 'fake-test-key', 'private_key_id': 'a'*40}))
                key.chmod(0o644 if unsafe == 'permissions' else 0o600)
                if unsafe == 'symlink':
                    actual = out/'actual.json'
                    key.rename(actual)
                    key.symlink_to(actual)
                if unsafe == 'hardlink': os.link(key, out/'hardlink.json')
                result = self.run_script('--project-id', 'demo-chat-123', '--endpoint-url', 'https://chat.example.com/webhook/googlechat',
                                         '--out', str(out), env=env)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('existing key is unsafe or mismatched', result.stderr)
                self.assertFalse((out/'bootstrap-result.json').exists())

    def test_refuses_symlink_output(self):
        with tempfile.TemporaryDirectory() as td:
            env = self.fake_environment(td)
            out = Path(td)/'link'
            out.symlink_to(td)
            result = self.run_script('--project-id', 'demo-chat-123', '--endpoint-url', 'https://chat.example.com/webhook/googlechat',
                                     '--out', str(out), env=env)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('unsafe output directory', result.stderr)


if __name__ == '__main__':
    unittest.main()
