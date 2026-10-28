"""Pure cleanup-policy tests. No process signaling or browser is invoked."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('native_cleanup', Path(__file__).with_name('local-browser-native-cleanup.py'))
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


def owned(pid=501, birth=10):
    return dict(pid=pid, group=501, birth_sec=birth, birth_usec=1,
                executable='/Applications/Google Chrome.app/Contents/MacOS/Google Chrome')


class CleanupPolicy(unittest.TestCase):
    def test_reused_pid_is_rejected_before_any_signal(self):
        with patch.object(MODULE, 'members', return_value=[owned(birth=11)]), patch.object(MODULE.os, 'kill') as kill:
            with self.assertRaisesRegex(RuntimeError, 'reused'):
                MODULE.inspect(501, {501: owned()})
            kill.assert_not_called()

    def test_unproven_survivor_without_a_live_anchor_is_not_adopted(self):
        with patch.object(MODULE, 'members', return_value=[owned(pid=502)]), patch.object(MODULE.os, 'kill') as kill:
            with self.assertRaisesRegex(RuntimeError, 'unproven'):
                MODULE.inspect(501, {501: owned()})
            kill.assert_not_called()

    def test_a_live_birth_bound_group_anchor_can_track_its_new_child(self):
        known = {501: owned()}
        with patch.object(MODULE, 'members', return_value=[owned(), owned(pid=502)]):
            self.assertEqual(len(MODULE.inspect(501, known)), 2)
        self.assertEqual(known[502], owned(pid=502))

    def test_duplicate_pid_in_manifest_cannot_replace_the_original_birth_proof(self):
        with tempfile.TemporaryDirectory(prefix='ddc-b9b-native-unit-') as root:
            path = Path(root, 'owned-processes.json')
            path.write_text(json.dumps({'group': 501, 'processes': [owned(), owned(birth=11)]}))
            with self.assertRaisesRegex(RuntimeError, 'duplicate'):
                MODULE.load_manifest(path)


if __name__ == '__main__':
    unittest.main()
