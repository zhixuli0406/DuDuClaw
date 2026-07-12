"""
tests/test_fetch_benchmarks.py
fetch_benchmarks M1 — 誠實 fail 行為單測（離線，不觸網）

驗證重點：無 --hf-repo / 無 datasets / repo 錯誤時，一律 raise FetchError
帶清楚指令，且 CLI 以非零退出，絕不捏造假資料集。
"""
from __future__ import annotations

import pytest

from duduclaw.memory_eval import fetch_benchmarks as fb


def test_longmemeval_requires_hf_repo():
    """LongMemEval 未給 --hf-repo → FetchError 指引查官方 repo。"""
    with pytest.raises(fb.FetchError, match="hf-repo"):
        fb.fetch_longmemeval(hf_repo=None)


def test_first_field_candidate_selection():
    """_first_field 取第一個存在且非空的候選欄位。"""
    row = {"a": "", "b": None, "c": "hit", "d": "later"}
    assert fb._first_field(row, ["a", "b", "c", "d"]) == "hit"
    assert fb._first_field(row, ["x", "y"]) is None


def test_require_datasets_honest_fail(monkeypatch):
    """缺 datasets 套件 → FetchError 指引 pip install。"""
    import builtins
    real_import = builtins.__import__

    def fake_import(name, *args, **kwargs):
        if name == "datasets":
            raise ImportError("no datasets")
        return real_import(name, *args, **kwargs)

    monkeypatch.setattr(builtins, "__import__", fake_import)
    with pytest.raises(fb.FetchError, match="pip install datasets"):
        fb._require_datasets()


def test_cli_longmemeval_no_repo_returns_nonzero(capsys):
    """CLI 無 repo → 非零退出、stderr 印 PENDING-LIVE，不寫檔。"""
    rc = fb.main(["longmemeval"])
    assert rc == 1
    err = capsys.readouterr().err
    assert "PENDING-LIVE" in err
