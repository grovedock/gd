"""A small, typed client over Grovedock's gRPC API."""

from __future__ import annotations

from dataclasses import dataclass
from urllib.parse import urlparse

import blake3
import grpc

from ._gen import gd_objects_pb2 as objects
from ._gen import gd_pb2 as pb
from ._gen import gd_pb2_grpc as rpc

_MAX_MESSAGE = 260 << 20
_CHANGESET_TAG = b"\x03"
_MODES = {0: "file", 1: "exec", 2: "symlink", 3: "dir"}


class IntegrityError(Exception):
    """The server returned data that does not match its content id."""


@dataclass(frozen=True)
class FileChange:
    path: str
    blob: str | None  # hex blob id, None for a deletion
    mode: str


@dataclass(frozen=True)
class Changeset:
    id: str
    parents: list[str]
    author: str
    message: str
    timestamp_ms: int
    changes: list[FileChange]
    redacted: bool

    @property
    def summary(self) -> str:
        return self.message.splitlines()[0] if self.message else ""


@dataclass(frozen=True)
class DirEntry:
    name: str
    mode: str
    id: str | None  # None for directories you may traverse but not read


def _change(c) -> FileChange:
    return FileChange(path=c.path, blob=c.blob.hex() or None, mode=_MODES.get(c.mode, "file"))


def _changeset(info: pb.ChangesetInfo) -> Changeset:
    if not info.redacted and blake3.blake3(_CHANGESET_TAG + info.encoded).digest() != info.id:
        raise IntegrityError(f"changeset {info.id.hex()} does not match its content")
    cs = objects.Changeset.FromString(info.encoded)
    return Changeset(
        id=info.id.hex(),
        parents=[p.hex() for p in cs.parents],
        author=cs.author,
        message=cs.message,
        timestamp_ms=cs.timestamp_ms,
        changes=[_change(c) for c in cs.changes],
        redacted=info.redacted,
    )


class Client:
    """Connect to a Grovedock server, e.g. ``Client("https://grovedock.example.com", token=...)``."""

    def __init__(self, url: str, token: str | None = None):
        parsed = urlparse(url)
        target = parsed.netloc or url
        options = [("grpc.max_receive_message_length", _MAX_MESSAGE), ("grpc.max_send_message_length", _MAX_MESSAGE)]
        if parsed.scheme == "https":
            self._channel = grpc.secure_channel(target, grpc.ssl_channel_credentials(), options)
        else:
            self._channel = grpc.insecure_channel(target, options)
        self._md = [("authorization", f"Bearer {token}")] if token else []
        self._browse = rpc.BrowseStub(self._channel)
        self._history = rpc.HistoryStub(self._channel)
        self._changesets = rpc.ChangesetsStub(self._channel)
        self._diff = rpc.DiffStub(self._channel)
        self._refs = rpc.RefsStub(self._channel)
        self._merge_base = rpc.MergeBaseStub(self._channel)
        self._access = rpc.AccessStub(self._channel)

    def close(self) -> None:
        self._channel.close()

    def __enter__(self) -> Client:
        return self

    def __exit__(self, *exc) -> None:
        self.close()

    def read_file(self, path: str, rev: str = "main") -> bytes:
        r = self._browse.ReadFile(pb.BrowseRequest(rev=rev, path=path), metadata=self._md)
        if blake3.blake3(b"\x01" + r.content).digest() != r.id:
            raise IntegrityError(f"{path} does not match its content id")
        return r.content

    def list_dir(self, path: str = "", rev: str = "main") -> list[DirEntry]:
        r = self._browse.ListDir(pb.BrowseRequest(rev=rev, path=path), metadata=self._md)
        return [DirEntry(e.name, _MODES.get(e.mode, "file"), e.id.hex() or None) for e in r.entries]

    def find(self, pattern: str, rev: str = "main", limit: int = 1000) -> list[str]:
        return list(self._browse.Find(pb.FindRequest(rev=rev, pattern=pattern, limit=limit), metadata=self._md).paths)

    def log(self, rev: str = "main", path: str = "", limit: int = 50) -> list[Changeset]:
        stream = self._history.Log(pb.LogRequest(rev=rev, path=path, limit=limit), metadata=self._md)
        return [_changeset(info) for info in stream]

    def changeset(self, rev: str) -> Changeset:
        return _changeset(self._changesets.Get(pb.ResolveRequest(rev=rev), metadata=self._md))

    def diff(self, from_rev: str, to_rev: str) -> list[FileChange]:
        r = self._diff.Diff(pb.DiffRequest(**{"from": from_rev, "to": to_rev}), metadata=self._md)
        return [_change(c) for c in r.changes]

    def blame(self, path: str, rev: str = "main") -> list[tuple[str, str]]:
        r = self._history.Blame(pb.BlameRequest(rev=rev, path=path), metadata=self._md)
        return [(line.changeset.hex(), line.text) for line in r.lines]

    def refs(self) -> dict[str, str]:
        return {e.name: e.id.hex() for e in self._refs.List(pb.Empty(), metadata=self._md).refs}

    def merge_base(self, a: str, b: str) -> str | None:
        r = self._merge_base.MergeBase(pb.MergeBaseRequest(a=a, b=b), metadata=self._md)
        return r.id.hex() if r.found else None

    def whoami(self) -> dict:
        r = self._access.WhoAmI(pb.Empty(), metadata=self._md)
        return {"principal": r.principal, "full_read": r.full_read, "read_prefixes": list(r.read_prefixes)}
