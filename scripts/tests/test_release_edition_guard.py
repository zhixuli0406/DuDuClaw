"""Keep fixture elevation out of official release feature selections."""
from pathlib import Path
import re
import tomllib
import unittest

ROOT = Path(__file__).resolve().parents[2]


class ReleaseEditionGuardTests(unittest.TestCase):
    def test_release_recipes_do_not_enable_fixture_features(self):
        recipes = list((ROOT / ".github/workflows").glob("*release*.yml"))
        recipes += list((ROOT / "scripts").glob("release*.sh"))
        self.assertTrue(recipes)
        for path in recipes:
            with self.subTest(path=str(path.relative_to(ROOT))):
                text = path.read_text()
                self.assertNotIn("test-edition-override", text)
                self.assertIsNone(re.search(r"--all-features\b", text))

    def test_fixture_feature_is_non_default(self):
        manifest = tomllib.loads((ROOT / "crates/duduclaw-core/Cargo.toml").read_text())
        features = manifest["features"]
        self.assertIn("test-edition-override", features)
        pending = list(features["default"])
        seen = set()
        while pending:
            feature = pending.pop()
            self.assertNotEqual(feature, "test-edition-override")
            if feature not in seen:
                seen.add(feature)
                pending.extend(features.get(feature, []))


if __name__ == "__main__":
    unittest.main()
