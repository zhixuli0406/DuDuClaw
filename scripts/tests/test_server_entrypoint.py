import os
import subprocess
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
ENTRYPOINT = ROOT / "container" / "server-entrypoint.sh"


class ServerEntrypointTests(unittest.TestCase):
    def run_entrypoint(
        self, *, initial_port=None, explicit_port=None, existing_config=False
    ):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            home = root / "home"
            bin_dir = root / "bin"
            home.mkdir()
            bin_dir.mkdir()
            capture = root / "capture"
            if existing_config:
                config_dir = home / ".duduclaw"
                config_dir.mkdir()
                (config_dir / "config.toml").write_text(
                    "[gateway]\nport = 19999\n"
                )
            fake = bin_dir / "duduclaw"
            fake.write_text(
                "#!/bin/sh\nprintf '%s' \"${DUDUCLAW_PORT-unset}\" > \"$CAPTURE\"\n"
            )
            fake.chmod(0o755)
            env = os.environ.copy()
            env.update({
                "HOME": str(home),
                "PATH": f"{bin_dir}:{env['PATH']}",
                "CAPTURE": str(capture),
            })
            env.pop("DUDUCLAW_INITIAL_PORT", None)
            env.pop("DUDUCLAW_PORT", None)
            if initial_port is not None:
                env["DUDUCLAW_INITIAL_PORT"] = str(initial_port)
            if explicit_port is not None:
                env["DUDUCLAW_PORT"] = str(explicit_port)
            subprocess.run(
                ["bash", str(ENTRYPOINT), "true"],
                check=True,
                env=env,
                capture_output=True,
                text=True,
            )
            return capture.read_text() if capture.exists() else None

    def test_persisted_platform_can_seed_legacy_port_on_first_run(self):
        self.assertEqual(self.run_entrypoint(initial_port=18789), "18789")

    def test_stateless_first_run_inherits_the_binary_default(self):
        self.assertEqual(self.run_entrypoint(), "unset")

    def test_explicit_port_wins_over_platform_initial_port(self):
        self.assertEqual(
            self.run_entrypoint(initial_port=18789, explicit_port=19999),
            "19999",
        )

    def test_existing_config_is_never_overridden(self):
        self.assertIsNone(
            self.run_entrypoint(initial_port=18789, existing_config=True)
        )


if __name__ == "__main__":
    unittest.main()
