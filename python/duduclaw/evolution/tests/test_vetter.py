"""Unit tests for SkillVetter secret detection (L25 regression)."""
from __future__ import annotations

from duduclaw.evolution.vetter import SkillVetter, VetterResult


def test_detects_github_fine_grained_pat():
    """github_pat_* fine-grained tokens must be flagged as FAIL."""
    content = "token = github_pat_11ABCDEFG0abcdefghijklmnopqrstuvwxyz1234567890"
    result, findings = SkillVetter().vet_skill("x", content)
    assert result == VetterResult.FAIL
    assert any(f.category == "sensitive_data" for f in findings)


def test_detects_github_classic_token_still_works():
    content = "ghp_" + "a" * 36
    result, _ = SkillVetter().vet_skill("x", content)
    assert result == VetterResult.FAIL


def test_clean_skill_passes():
    result, findings = SkillVetter().vet_skill("x", "# A harmless skill\nprint great")
    assert result == VetterResult.PASS
    assert findings == []
