"""Exercise conservative CI decisions against real Git histories."""

import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch


SCRIPTS = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(SCRIPTS))
import ci_classify
import ci_warm_needed


LOCK = '''version = 4

[[package]]
name = "duduclaw-core"
version = "1.79.0"
dependencies = ["serde"]

[[package]]
name = "serde"
version = "1.0.200"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "unchanged"
'''


class CiClassifyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.previous = Path.cwd()
        self.repo = Path(self.temporary.name)
        os.chdir(self.repo)
        self.addCleanup(os.chdir, self.previous)
        self.git("init", "-q")
        self.git("config", "user.name", "CI Test")
        self.git("config", "user.email", "ci@example.invalid")
        self.git("config", "commit.gpgsign", "false")
        self.write("Cargo.toml", '[workspace.package]\nversion = "1.79.0"\n')
        self.write("Cargo.lock", LOCK)
        self.write("src-tauri/Cargo.lock", LOCK)
        self.write("CHANGELOG.md", "# Changelog\n\n## [Unreleased]\n\n### Fixed\n\n- Existing fix.\n")
        self.base = self.commit()

    def git(self, *args):
        return subprocess.run(["git", *args], check=True, capture_output=True,
                              encoding="utf-8").stdout.strip()

    def write(self, path, text):
        target = self.repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")

    def commit(self):
        self.git("add", ".")
        self.git("commit", "-qm", "fixture")
        return self.git("rev-parse", "HEAD")

    def classify(self):
        return ci_classify.classify("pull_request", self.base, self.commit())

    def bump(self):
        self.write("distribution/release-delivery/v1.79.1.json", "{}\n")
        self.write("distribution/release-delivery/v1.79.1.sig", "signature\n")
        self.write("Cargo.toml", '[workspace.package]\nversion = "1.79.1"\n')
        self.write("Cargo.lock", LOCK.replace('version = "1.79.0"', 'version = "1.79.1"'))
        self.write("src-tauri/Cargo.lock", LOCK.replace('version = "1.79.0"', 'version = "1.79.1"'))
        self.write("CHANGELOG.md", "# Changelog\n\n## [Unreleased]\n\n## [1.79.1] - 2026-11-03\n\n### Fixed\n\n- Existing fix.\n")

    def test_docs_only_skip(self):
        for path in ("docs/guide.md", "wiki/start.md", "marketing/copy.txt", "team/plan.txt", "NOTES.md"):
            self.write(path, "Documentation\n")
        self.assertEqual(self.classify(), dict(rust_mode="skip", dashboard="skip",
                                              app_compat="false"))

    def test_web_only_runs_dashboard(self):
        self.write("web/src/main.ts", "export {};\n")
        self.assertEqual(self.classify(), dict(rust_mode="skip", dashboard="run",
                                              app_compat="false"))

    def test_mixed_web_and_docs_runs_dashboard(self):
        self.write("web/src/main.ts", "export {};\n")
        self.write("docs/guide.md", "Documentation\n")
        result = self.classify()
        self.assertEqual((result["rust_mode"], result["dashboard"]), ("skip", "run"))

    def test_rust_consumed_docs_force_full(self):
        for path in ("docs/README.md", "docs/assets/example.json"):
            with self.subTest(path=path):
                self.git("checkout", "--detach", self.base)
                self.write(path, "Required fixture\n")
                result = self.classify()
                self.assertEqual((result["rust_mode"], result["dashboard"]), ("full", "run"))

    def test_real_bump_file_set_and_shape_checks(self):
        # Replay the 21-file shape of the actual v1.79.0 release bump.
        files = {
            "pyproject.toml": '[project]\nversion = "1.79.0"\n',
            "python/duduclaw/__init__.py": '__version__ = "1.79.0"\n',
            "npm/duduclaw/package.json": '{"version":"1.79.0",\n"optionalDependencies":{'
                + ','.join(f'"@duduclaw/{platform}":"1.79.0"' for platform in
                           ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"))
                + '}}\n',
            "scripts/install.sh": 'FALLBACK_VERSION="1.79.0"\n',
            "scripts/install.ps1": '$FallbackVersion = "1.79.0"\n',
            "src-tauri/tauri.conf.json": '{"version":"1.79.0"}\n',
            "distribution/registries/mcp/server.json":
                '{"version":"1.79.0","packages":[{"version":"1.79.0"}]}\n',
        }
        for platform in ("darwin-arm64", "darwin-x64", "linux-arm64", "linux-x64", "win32-x64"):
            files[f"npm/{platform}/package.json"] = '{"version":"1.79.0"}\n'
        for path in ("README.md", "README.zh-TW.md", "README.ja.md"):
            files[path] = "Version badge: 1.79.0\n"
        # The desktop lockfile can retain local packages at older versions.
        desktop_lock = LOCK + '\n[[package]]\nname = "duduclaw-desktop"\nversion = "1.0.0"\n'
        self.write("src-tauri/Cargo.lock", desktop_lock)
        for path, text in files.items():
            self.write(path, text)
        self.base = self.commit()
        self.bump()
        self.write("src-tauri/Cargo.lock", desktop_lock.replace('version = "1.79.0"', 'version = "1.79.1"'))
        for path, text in files.items():
            self.write(path, text.replace("1.79.0", "1.79.1"))
        head = self.commit()
        paths = self.git("diff", "--name-only", self.base, head).splitlines()
        self.assertEqual(len(paths), 21)
        self.assertEqual(ci_classify.classify("pull_request", self.base, head),
                         dict(rust_mode="check", dashboard="skip",
                              app_compat="false"))

    def test_version_change_without_changelog_forces_full(self):
        self.write("Cargo.toml", '[workspace.package]\nversion = "1.79.1"\n')
        self.assertEqual(self.classify(), ci_classify.FULL)

    def test_malformed_lock_forces_full_and_warming(self):
        self.bump()
        self.write("Cargo.lock", LOCK.replace('version = "1.79.0"', 'version = "1.79.1'))
        head = self.commit()
        self.assertEqual(ci_classify.classify("pull_request", self.base, head), ci_classify.FULL)
        self.assertTrue(ci_warm_needed.warm_needed("push", head))

    def test_tags_and_urls_must_use_exact_workspace_bump(self):
        self.write("scripts/install.sh", 'TAG="v1.79.0"\n')
        self.write("README.md", "Download https://example.invalid/v1.79.0/file\n")
        self.base = self.commit()
        self.bump()
        self.write("scripts/install.sh", 'TAG="v1.79.1"\n')
        self.write("README.md", "Download https://example.invalid/v1.79.1/file\n")
        self.assertEqual(self.classify()["rust_mode"], "check")

    def test_unrelated_semver_edits_force_full(self):
        original_base = self.base
        for path, old, new in (
            ("scripts/install.sh", 'FALLBACK_VERSION="1.79.0"\n', 'FALLBACK_VERSION="0.0.0"\n'),
            ("Cargo.toml", '[workspace.package]\nversion = "1.79.0"\n'
             '[workspace.dependencies]\nserde = "1.0.200"\n',
             '[workspace.package]\nversion = "1.79.1"\n'
             '[workspace.dependencies]\nserde = "1.0.201"\n'),
            ("crates/duduclaw-core/Cargo.toml", '[dependencies]\nserde = "1.0.200"\n',
             '[dependencies]\nserde = "1.0.201"\n'),
            ("npm/duduclaw/package.json", '{"version":"1.79.0","optionalDependencies":{"pkg":"1.79.0"}}\n',
             '{"version":"1.79.1","optionalDependencies":{"pkg":"1.80.0"}}\n'),
            ("scripts/install.sh", 'TAG="v1.79.0"\nURL="https://example.invalid/v1.79.0/file"\n',
             'TAG="v1.79.1"\nURL="https://example.invalid/v1.80.0/file"\n'),
        ):
            with self.subTest(path=path):
                self.base = original_base
                self.git("checkout", "--detach", self.base)
                self.write(path, old)
                self.base = self.commit()
                self.bump()
                self.write(path, new)
                self.assertEqual(self.classify()["rust_mode"], "full")

    def test_lower_or_unchanged_workspace_version_forces_full(self):
        for version in ("1.78.9", "1.79.0"):
            with self.subTest(version=version):
                self.bump()
                self.write("Cargo.toml", f'[workspace.package]\nversion = "{version}"\n')
                self.assertEqual(self.classify()["rust_mode"], "full")

    def test_numeric_version_order(self):
        self.bump()
        for path in ("Cargo.toml", "Cargo.lock", "src-tauri/Cargo.lock", "CHANGELOG.md"):
            target = self.repo / path
            target.write_text(target.read_text().replace("1.79.1", "1.100.0"))
        for ext in ("json", "sig"):
            (self.repo / f"distribution/release-delivery/v1.79.1.{ext}").rename(
                self.repo / f"distribution/release-delivery/v1.100.0.{ext}")
        self.assertEqual(self.classify()["rust_mode"], "check")

    def test_missing_or_incorrect_delivery_files_force_full(self):
        for missing in (("json",), ("sig",), ("json", "sig")):
            with self.subTest(missing=missing):
                self.bump()
                for ext in missing:
                    (self.repo / f"distribution/release-delivery/v1.79.1.{ext}").unlink()
                self.assertEqual(self.classify()["rust_mode"], "full")
        self.bump()
        (self.repo / "distribution/release-delivery/v1.79.1.sig").rename(
            self.repo / "distribution/release-delivery/v1.79.1.json.sig")
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_changelog_requires_new_matching_header(self):
        self.bump()
        self.write("CHANGELOG.md", (self.repo / "CHANGELOG.md").read_text().replace("[1.79.1]", "[1.80.0]"))
        self.assertEqual(self.classify()["rust_mode"], "full")
        self.git("checkout", "--detach", self.base)
        self.write("CHANGELOG.md", (self.repo / "CHANGELOG.md").read_text() + "\n## [1.79.1]\n")
        self.base = self.commit()
        self.bump()
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_lock_workspace_versions_must_be_exact_old_to_new(self):
        for path in ("Cargo.lock", "src-tauri/Cargo.lock"):
            for version in ("0.0.0", "1.80.0"):
                with self.subTest(path=path, version=version):
                    self.bump()
                    self.write(path, LOCK.replace("1.79.0", version))
                    self.assertEqual(self.classify()["rust_mode"], "full")

    def test_readmes_allow_documentation_changes_during_bump(self):
        for path in ("README.md", "README.zh-TW.md", "README.ja.md"):
            self.write(path, "Old documentation\n")
        self.base = self.commit()
        self.bump()
        for path in ("README.md", "README.zh-TW.md", "README.ja.md"):
            self.write(path, "New documentation\nwith another line\n")
        self.assertEqual(self.classify()["rust_mode"], "check")

    def test_third_party_lock_change_forces_full(self):
        self.bump()
        self.write("Cargo.lock", LOCK.replace("1.0.200", "1.0.201"))
        self.assertEqual(self.classify(), ci_classify.FULL)

    def test_code_change_disguised_as_version_edit_forces_full(self):
        self.write("python/duduclaw/__init__.py", '__version__ = "1.79.0"\n')
        self.base = self.commit()
        self.bump()
        self.write("python/duduclaw/__init__.py", '__version__ = "1.79.1"\nprint("new code")\n')
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_changelog_body_change_forces_full(self):
        self.bump()
        self.write("CHANGELOG.md", "## [1.79.1]\n\n- New behavior.\n")
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_bump_file_addition_or_deletion_forces_full(self):
        self.bump()
        self.write("scripts/install.sh", 'VERSION="1.79.1"\n')
        self.assertEqual(self.classify()["rust_mode"], "full")
        (self.repo / "Cargo.toml").unlink()
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_version_file_mode_change_forces_full(self):
        self.bump()
        self.git("add", "Cargo.toml")
        self.git("update-index", "--chmod=+x", "Cargo.toml")
        self.git("commit", "-qm", "mode change")
        head = self.git("rev-parse", "HEAD")
        self.assertEqual(ci_classify.classify("pull_request", self.base, head)["rust_mode"], "full")

    def test_sensitive_roots_force_full(self):
        for path in ("scripts/check.py", ".github/actions/example/action.yml",
                     "vendor/example/lib.rs", "src-tauri/src/main.rs"):
            with self.subTest(path=path):
                self.git("checkout", "--detach", self.base)
                self.write(path, "Changed behavior\n")
                self.assertEqual(self.classify()["rust_mode"], "full")

    def test_unknown_path_fails_toward_all_coverage(self):
        self.write("new-root/unknown.txt", "Unknown\n")
        self.assertEqual(self.classify(), ci_classify.FULL)

    def test_missing_or_invalid_commit_fails_toward_all_coverage(self):
        for base, head in (("", self.base), (self.base, ""),
                           ("--help", self.base), (self.base, "0" * 40)):
            with self.subTest(base=base, head=head):
                self.assertEqual(ci_classify.classify("pull_request", base, head), ci_classify.FULL)
        with patch.object(ci_classify, "git", side_effect=OSError("git unavailable")):
            self.assertEqual(ci_classify.classify("pull_request", self.base, self.base), ci_classify.FULL)

    def test_empty_diff_fails_toward_all_coverage(self):
        self.assertEqual(ci_classify.classify("pull_request", self.base, self.base), ci_classify.FULL)

    def test_non_pr_events_always_run_full(self):
        self.write("docs/guide.md", "Documentation\n")
        head = self.commit()
        for event in ("push", "schedule", "workflow_dispatch", ""):
            with self.subTest(event=event):
                self.assertEqual(ci_classify.classify(event, self.base, head), ci_classify.FULL)

    def test_rename_cannot_hide_rust_deletion(self):
        self.write("crates/duduclaw-core/src/example.rs", "// Example\n")
        self.base = self.commit()
        self.git("mv", "crates/duduclaw-core/src/example.rs", "EXAMPLE.md")
        self.assertEqual(self.classify()["rust_mode"], "full")

    def test_merge_base_ignores_base_branch_only_changes(self):
        self.git("checkout", "-qb", "pr")
        self.write("docs/guide.md", "Documentation\n")
        head = self.commit()
        self.git("checkout", "--detach", self.base)
        self.write("crates/duduclaw-core/src/example.rs", "// Base-only change\n")
        base = self.commit()
        result = ci_classify.classify("pull_request", base, head)
        self.assertEqual((result["rust_mode"], result["dashboard"]), ("skip", "skip"))

    def test_app_compat_path_filters(self):
        for path, compat in (
            ("crates/duduclaw-core/src/compat.rs", "true"),
            ("crates/duduclaw-cli/src/compat.rs", "true"),
            ("crates/duduclaw-core/src/normal.rs", "false"),
            ("Cargo.toml", "true"),
            ("Cargo.lock", "true"),
            ("crates/duduclaw-gateway/Cargo.toml", "true"),
            ("vendor/example/lib.rs", "true"),
            (".github/workflows/ci.yml", "true"),
        ):
            with self.subTest(path=path):
                self.git("checkout", "--detach", self.base)
                self.write(path, "Changed behavior\n")
                result = self.classify()
                self.assertEqual((result["rust_mode"], result["app_compat"]),
                                 ("full", compat))

    def test_cli_writes_github_output_with_env_shas(self):
        self.write("docs/guide.md", "Documentation\n")
        head = self.commit()
        output = self.repo / "outputs"
        subprocess.run([sys.executable, str(SCRIPTS / "ci_classify.py")], check=True,
                       env={**os.environ, "GITHUB_EVENT_NAME": "pull_request",
                            "BASE_SHA": self.base, "HEAD_SHA": head, "GITHUB_OUTPUT": str(output)})
        self.assertEqual(dict(line.split("=", 1) for line in output.read_text().splitlines()),
                         dict(rust_mode="skip", dashboard="skip", app_compat="false"))

    def test_warm_runs_for_manifest_change_without_lock_diff(self):
        self.write("Cargo.toml", '[workspace.package]\nversion = "1.79.0"\n\n[profile.release]\nopt-level = 2\n')
        self.assertTrue(ci_warm_needed.warm_needed("push", self.commit()))

    def test_warm_skips_workspace_version_only_bump(self):
        self.bump()
        self.assertFalse(ci_warm_needed.warm_needed("push", self.commit()))

    def test_warm_runs_for_dependency_changes_in_either_lock(self):
        for path in ("Cargo.lock", "src-tauri/Cargo.lock"):
            with self.subTest(path=path):
                self.write(path, LOCK.replace("1.0.200", "1.0.201"))
                self.assertTrue(ci_warm_needed.warm_needed("push", self.commit()))

    def test_warm_always_runs_for_non_push_and_errors(self):
        for event in ("schedule", "workflow_dispatch", "pull_request", ""):
            self.assertTrue(ci_warm_needed.warm_needed(event, self.base))
        for head in (self.base, "", "0" * 40):
            self.assertTrue(ci_warm_needed.warm_needed("push", head))
        self.bump()
        (self.repo / "src-tauri/Cargo.lock").unlink()
        self.assertTrue(ci_warm_needed.warm_needed("push", self.commit()))

    def test_lock_source_and_non_version_fields_are_not_masked(self):
        for new in (LOCK.replace('checksum = "unchanged"', 'checksum = "changed"'),
                    LOCK.replace('dependencies = ["serde"]', 'dependencies = []'),
                    LOCK.replace('name = "duduclaw-core"', 'name = "other"')):
            self.assertFalse(ci_classify.lock_versions_only(LOCK, new))
        registry = LOCK.replace('name = "duduclaw-core"',
                                'name = "duduclaw-core"\nsource = "registry+example"')
        self.assertFalse(ci_classify.lock_versions_only(registry, registry.replace("1.79.0", "1.79.1")))


if __name__ == "__main__":
    unittest.main()
