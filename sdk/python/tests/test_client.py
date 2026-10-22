"""Runs against a seeded server: eval "$(../test-server.sh)" (sets GD_URL)."""

import os

import pytest

from grovedock import Client

URL = os.environ.get("GD_URL")
pytestmark = pytest.mark.skipif(not URL, reason="GD_URL not set (start ../test-server.sh)")


@pytest.fixture
def repo():
    with Client(URL) as c:
        yield c


def test_reads_without_a_clone(repo):
    assert repo.read_file("README.md") == b"hello grovedock\n"
    assert repo.read_file("src/lib.rs") == b"pub fn answer() -> u32 { 42 }\n"
    assert [e.name for e in repo.list_dir()] == ["README.md", "docs", "src"]
    assert repo.find("guide") == ["docs/guide.md"]


def test_history_is_verified_and_decoded(repo):
    log = repo.log()
    assert [c.summary for c in log] == ["fix the answer", "initial"]
    assert log[0].author == "SDK Test <sdk@example.com>" and not log[0].redacted
    assert log[0].parents == [log[1].id]
    assert {c.path for c in log[0].changes} == {"src/lib.rs", "docs/guide.md"}
    assert repo.changeset(log[1].id).summary == "initial"
    assert [c.summary for c in repo.log(path="docs")] == ["fix the answer"]


def test_diff_blame_refs(repo):
    first, latest = repo.log()[1].id, repo.log()[0].id
    assert {c.path for c in repo.diff(first, latest)} == {"src/lib.rs", "docs/guide.md"}
    owners = repo.blame("src/lib.rs")
    assert owners == [(latest, "pub fn answer() -> u32 { 42 }")]
    assert repo.refs() == {"main": latest}
    assert repo.merge_base(first, latest) == first
    assert repo.whoami()["full_read"] is True
