from google.protobuf.internal import containers as _containers
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Empty(_message.Message):
    __slots__ = ()
    def __init__(self) -> None: ...

class IdMsg(_message.Message):
    __slots__ = ("id",)
    ID_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    def __init__(self, id: _Optional[bytes] = ...) -> None: ...

class IdList(_message.Message):
    __slots__ = ("ids",)
    IDS_FIELD_NUMBER: _ClassVar[int]
    ids: _containers.RepeatedScalarFieldContainer[bytes]
    def __init__(self, ids: _Optional[_Iterable[bytes]] = ...) -> None: ...

class ObjectPayload(_message.Message):
    __slots__ = ("id", "payload")
    ID_FIELD_NUMBER: _ClassVar[int]
    PAYLOAD_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    payload: bytes
    def __init__(self, id: _Optional[bytes] = ..., payload: _Optional[bytes] = ...) -> None: ...

class PutObjectsResponse(_message.Message):
    __slots__ = ("stored",)
    STORED_FIELD_NUMBER: _ClassVar[int]
    stored: int
    def __init__(self, stored: _Optional[int] = ...) -> None: ...

class FileChange(_message.Message):
    __slots__ = ("path", "blob", "mode")
    PATH_FIELD_NUMBER: _ClassVar[int]
    BLOB_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    path: str
    blob: bytes
    mode: int
    def __init__(self, path: _Optional[str] = ..., blob: _Optional[bytes] = ..., mode: _Optional[int] = ...) -> None: ...

class ChangesetInfo(_message.Message):
    __slots__ = ("id", "root_tree", "generation", "encoded", "redacted", "provenance")
    ID_FIELD_NUMBER: _ClassVar[int]
    ROOT_TREE_FIELD_NUMBER: _ClassVar[int]
    GENERATION_FIELD_NUMBER: _ClassVar[int]
    ENCODED_FIELD_NUMBER: _ClassVar[int]
    REDACTED_FIELD_NUMBER: _ClassVar[int]
    PROVENANCE_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    root_tree: bytes
    generation: int
    encoded: bytes
    redacted: bool
    provenance: Provenance
    def __init__(self, id: _Optional[bytes] = ..., root_tree: _Optional[bytes] = ..., generation: _Optional[int] = ..., encoded: _Optional[bytes] = ..., redacted: _Optional[bool] = ..., provenance: _Optional[_Union[Provenance, _Mapping]] = ...) -> None: ...

class Provenance(_message.Message):
    __slots__ = ("by", "agent", "model", "session", "task", "at_ms")
    BY_FIELD_NUMBER: _ClassVar[int]
    AGENT_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    SESSION_FIELD_NUMBER: _ClassVar[int]
    TASK_FIELD_NUMBER: _ClassVar[int]
    AT_MS_FIELD_NUMBER: _ClassVar[int]
    by: str
    agent: bool
    model: str
    session: str
    task: int
    at_ms: int
    def __init__(self, by: _Optional[str] = ..., agent: _Optional[bool] = ..., model: _Optional[str] = ..., session: _Optional[str] = ..., task: _Optional[int] = ..., at_ms: _Optional[int] = ...) -> None: ...

class CreateChangesetRequest(_message.Message):
    __slots__ = ("parents", "changes", "author", "message", "timestamp_ms", "git", "model", "session", "task", "parent_refs")
    PARENTS_FIELD_NUMBER: _ClassVar[int]
    CHANGES_FIELD_NUMBER: _ClassVar[int]
    AUTHOR_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    TIMESTAMP_MS_FIELD_NUMBER: _ClassVar[int]
    GIT_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    SESSION_FIELD_NUMBER: _ClassVar[int]
    TASK_FIELD_NUMBER: _ClassVar[int]
    PARENT_REFS_FIELD_NUMBER: _ClassVar[int]
    parents: _containers.RepeatedScalarFieldContainer[bytes]
    changes: _containers.RepeatedCompositeFieldContainer[FileChange]
    author: str
    message: str
    timestamp_ms: int
    git: bytes
    model: str
    session: str
    task: int
    parent_refs: _containers.RepeatedCompositeFieldContainer[ParentRef]
    def __init__(self, parents: _Optional[_Iterable[bytes]] = ..., changes: _Optional[_Iterable[_Union[FileChange, _Mapping]]] = ..., author: _Optional[str] = ..., message: _Optional[str] = ..., timestamp_ms: _Optional[int] = ..., git: _Optional[bytes] = ..., model: _Optional[str] = ..., session: _Optional[str] = ..., task: _Optional[int] = ..., parent_refs: _Optional[_Iterable[_Union[ParentRef, _Mapping]]] = ...) -> None: ...

class ParentRef(_message.Message):
    __slots__ = ("id", "earlier")
    ID_FIELD_NUMBER: _ClassVar[int]
    EARLIER_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    earlier: int
    def __init__(self, id: _Optional[bytes] = ..., earlier: _Optional[int] = ...) -> None: ...

class CreatedChangeset(_message.Message):
    __slots__ = ("id",)
    ID_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    def __init__(self, id: _Optional[bytes] = ...) -> None: ...

class ResolveRequest(_message.Message):
    __slots__ = ("rev",)
    REV_FIELD_NUMBER: _ClassVar[int]
    rev: str
    def __init__(self, rev: _Optional[str] = ...) -> None: ...

class RefName(_message.Message):
    __slots__ = ("name",)
    NAME_FIELD_NUMBER: _ClassVar[int]
    name: str
    def __init__(self, name: _Optional[str] = ...) -> None: ...

class RefValue(_message.Message):
    __slots__ = ("exists", "id")
    EXISTS_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    exists: bool
    id: bytes
    def __init__(self, exists: _Optional[bool] = ..., id: _Optional[bytes] = ...) -> None: ...

class CasRequest(_message.Message):
    __slots__ = ("name", "expect_exists", "expected", "new")
    NAME_FIELD_NUMBER: _ClassVar[int]
    EXPECT_EXISTS_FIELD_NUMBER: _ClassVar[int]
    EXPECTED_FIELD_NUMBER: _ClassVar[int]
    NEW_FIELD_NUMBER: _ClassVar[int]
    name: str
    expect_exists: bool
    expected: bytes
    new: bytes
    def __init__(self, name: _Optional[str] = ..., expect_exists: _Optional[bool] = ..., expected: _Optional[bytes] = ..., new: _Optional[bytes] = ...) -> None: ...

class CasResponse(_message.Message):
    __slots__ = ("swapped",)
    SWAPPED_FIELD_NUMBER: _ClassVar[int]
    swapped: bool
    def __init__(self, swapped: _Optional[bool] = ...) -> None: ...

class RefEntry(_message.Message):
    __slots__ = ("name", "id")
    NAME_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    name: str
    id: bytes
    def __init__(self, name: _Optional[str] = ..., id: _Optional[bytes] = ...) -> None: ...

class RefList(_message.Message):
    __slots__ = ("refs",)
    REFS_FIELD_NUMBER: _ClassVar[int]
    refs: _containers.RepeatedCompositeFieldContainer[RefEntry]
    def __init__(self, refs: _Optional[_Iterable[_Union[RefEntry, _Mapping]]] = ...) -> None: ...

class LandRequest(_message.Message):
    __slots__ = ("ref", "changeset", "fast_forward")
    REF_FIELD_NUMBER: _ClassVar[int]
    CHANGESET_FIELD_NUMBER: _ClassVar[int]
    FAST_FORWARD_FIELD_NUMBER: _ClassVar[int]
    ref: str
    changeset: bytes
    fast_forward: bool
    def __init__(self, ref: _Optional[str] = ..., changeset: _Optional[bytes] = ..., fast_forward: _Optional[bool] = ...) -> None: ...

class LandResponse(_message.Message):
    __slots__ = ("landed", "rebased", "attempts")
    LANDED_FIELD_NUMBER: _ClassVar[int]
    REBASED_FIELD_NUMBER: _ClassVar[int]
    ATTEMPTS_FIELD_NUMBER: _ClassVar[int]
    landed: bytes
    rebased: bool
    attempts: int
    def __init__(self, landed: _Optional[bytes] = ..., rebased: _Optional[bool] = ..., attempts: _Optional[int] = ...) -> None: ...

class LogRequest(_message.Message):
    __slots__ = ("rev", "path", "limit")
    REV_FIELD_NUMBER: _ClassVar[int]
    PATH_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    rev: str
    path: str
    limit: int
    def __init__(self, rev: _Optional[str] = ..., path: _Optional[str] = ..., limit: _Optional[int] = ...) -> None: ...

class BlameRequest(_message.Message):
    __slots__ = ("rev", "path")
    REV_FIELD_NUMBER: _ClassVar[int]
    PATH_FIELD_NUMBER: _ClassVar[int]
    rev: str
    path: str
    def __init__(self, rev: _Optional[str] = ..., path: _Optional[str] = ...) -> None: ...

class BlameLine(_message.Message):
    __slots__ = ("changeset", "text", "origin_line", "by")
    CHANGESET_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    ORIGIN_LINE_FIELD_NUMBER: _ClassVar[int]
    BY_FIELD_NUMBER: _ClassVar[int]
    changeset: bytes
    text: str
    origin_line: int
    by: str
    def __init__(self, changeset: _Optional[bytes] = ..., text: _Optional[str] = ..., origin_line: _Optional[int] = ..., by: _Optional[str] = ...) -> None: ...

class LineAuthor(_message.Message):
    __slots__ = ("agent", "name", "model", "person", "source")
    AGENT_FIELD_NUMBER: _ClassVar[int]
    NAME_FIELD_NUMBER: _ClassVar[int]
    MODEL_FIELD_NUMBER: _ClassVar[int]
    PERSON_FIELD_NUMBER: _ClassVar[int]
    SOURCE_FIELD_NUMBER: _ClassVar[int]
    agent: bool
    name: str
    model: str
    person: str
    source: str
    def __init__(self, agent: _Optional[bool] = ..., name: _Optional[str] = ..., model: _Optional[str] = ..., person: _Optional[str] = ..., source: _Optional[str] = ...) -> None: ...

class BlameResponse(_message.Message):
    __slots__ = ("lines", "authors")
    class AuthorsEntry(_message.Message):
        __slots__ = ("key", "value")
        KEY_FIELD_NUMBER: _ClassVar[int]
        VALUE_FIELD_NUMBER: _ClassVar[int]
        key: str
        value: LineAuthor
        def __init__(self, key: _Optional[str] = ..., value: _Optional[_Union[LineAuthor, _Mapping]] = ...) -> None: ...
    LINES_FIELD_NUMBER: _ClassVar[int]
    AUTHORS_FIELD_NUMBER: _ClassVar[int]
    lines: _containers.RepeatedCompositeFieldContainer[BlameLine]
    authors: _containers.MessageMap[str, LineAuthor]
    def __init__(self, lines: _Optional[_Iterable[_Union[BlameLine, _Mapping]]] = ..., authors: _Optional[_Mapping[str, LineAuthor]] = ...) -> None: ...

class AttachGitAiNoteRequest(_message.Message):
    __slots__ = ("changeset", "note")
    CHANGESET_FIELD_NUMBER: _ClassVar[int]
    NOTE_FIELD_NUMBER: _ClassVar[int]
    changeset: bytes
    note: str
    def __init__(self, changeset: _Optional[bytes] = ..., note: _Optional[str] = ...) -> None: ...

class GitAiNoteResponse(_message.Message):
    __slots__ = ("note",)
    NOTE_FIELD_NUMBER: _ClassVar[int]
    note: str
    def __init__(self, note: _Optional[str] = ...) -> None: ...

class DiffRequest(_message.Message):
    __slots__ = ("to",)
    FROM_FIELD_NUMBER: _ClassVar[int]
    TO_FIELD_NUMBER: _ClassVar[int]
    to: str
    def __init__(self, to: _Optional[str] = ..., **kwargs) -> None: ...

class DiffResponse(_message.Message):
    __slots__ = ("changes",)
    CHANGES_FIELD_NUMBER: _ClassVar[int]
    changes: _containers.RepeatedCompositeFieldContainer[FileChange]
    def __init__(self, changes: _Optional[_Iterable[_Union[FileChange, _Mapping]]] = ...) -> None: ...

class MergeBaseRequest(_message.Message):
    __slots__ = ("a", "b")
    A_FIELD_NUMBER: _ClassVar[int]
    B_FIELD_NUMBER: _ClassVar[int]
    a: str
    b: str
    def __init__(self, a: _Optional[str] = ..., b: _Optional[str] = ...) -> None: ...

class MergeBaseResponse(_message.Message):
    __slots__ = ("found", "id")
    FOUND_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    found: bool
    id: bytes
    def __init__(self, found: _Optional[bool] = ..., id: _Optional[bytes] = ...) -> None: ...

class BrowseRequest(_message.Message):
    __slots__ = ("rev", "path")
    REV_FIELD_NUMBER: _ClassVar[int]
    PATH_FIELD_NUMBER: _ClassVar[int]
    rev: str
    path: str
    def __init__(self, rev: _Optional[str] = ..., path: _Optional[str] = ...) -> None: ...

class DirEntry(_message.Message):
    __slots__ = ("name", "mode", "id")
    NAME_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    name: str
    mode: int
    id: bytes
    def __init__(self, name: _Optional[str] = ..., mode: _Optional[int] = ..., id: _Optional[bytes] = ...) -> None: ...

class ListDirResponse(_message.Message):
    __slots__ = ("entries",)
    ENTRIES_FIELD_NUMBER: _ClassVar[int]
    entries: _containers.RepeatedCompositeFieldContainer[DirEntry]
    def __init__(self, entries: _Optional[_Iterable[_Union[DirEntry, _Mapping]]] = ...) -> None: ...

class ReadFileResponse(_message.Message):
    __slots__ = ("content", "mode", "id")
    CONTENT_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    content: bytes
    mode: int
    id: bytes
    def __init__(self, content: _Optional[bytes] = ..., mode: _Optional[int] = ..., id: _Optional[bytes] = ...) -> None: ...

class FindRequest(_message.Message):
    __slots__ = ("rev", "pattern", "limit")
    REV_FIELD_NUMBER: _ClassVar[int]
    PATTERN_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    rev: str
    pattern: str
    limit: int
    def __init__(self, rev: _Optional[str] = ..., pattern: _Optional[str] = ..., limit: _Optional[int] = ...) -> None: ...

class FindResponse(_message.Message):
    __slots__ = ("paths", "truncated")
    PATHS_FIELD_NUMBER: _ClassVar[int]
    TRUNCATED_FIELD_NUMBER: _ClassVar[int]
    paths: _containers.RepeatedScalarFieldContainer[str]
    truncated: bool
    def __init__(self, paths: _Optional[_Iterable[str]] = ..., truncated: _Optional[bool] = ...) -> None: ...

class CheckoutRequest(_message.Message):
    __slots__ = ("rev", "paths")
    REV_FIELD_NUMBER: _ClassVar[int]
    PATHS_FIELD_NUMBER: _ClassVar[int]
    rev: str
    paths: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, rev: _Optional[str] = ..., paths: _Optional[_Iterable[str]] = ...) -> None: ...

class CheckoutPiece(_message.Message):
    __slots__ = ("path", "mode", "id", "data", "last")
    PATH_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    DATA_FIELD_NUMBER: _ClassVar[int]
    LAST_FIELD_NUMBER: _ClassVar[int]
    path: str
    mode: int
    id: bytes
    data: bytes
    last: bool
    def __init__(self, path: _Optional[str] = ..., mode: _Optional[int] = ..., id: _Optional[bytes] = ..., data: _Optional[bytes] = ..., last: _Optional[bool] = ...) -> None: ...

class SearchCodeRequest(_message.Message):
    __slots__ = ("query", "ref", "limit", "page_token")
    QUERY_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    PAGE_TOKEN_FIELD_NUMBER: _ClassVar[int]
    query: str
    ref: str
    limit: int
    page_token: str
    def __init__(self, query: _Optional[str] = ..., ref: _Optional[str] = ..., limit: _Optional[int] = ..., page_token: _Optional[str] = ...) -> None: ...

class CodeLine(_message.Message):
    __slots__ = ("line", "text", "ranges")
    LINE_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    RANGES_FIELD_NUMBER: _ClassVar[int]
    line: int
    text: str
    ranges: _containers.RepeatedScalarFieldContainer[int]
    def __init__(self, line: _Optional[int] = ..., text: _Optional[str] = ..., ranges: _Optional[_Iterable[int]] = ...) -> None: ...

class CodeHit(_message.Message):
    __slots__ = ("path", "lines", "more")
    PATH_FIELD_NUMBER: _ClassVar[int]
    LINES_FIELD_NUMBER: _ClassVar[int]
    MORE_FIELD_NUMBER: _ClassVar[int]
    path: str
    lines: _containers.RepeatedCompositeFieldContainer[CodeLine]
    more: int
    def __init__(self, path: _Optional[str] = ..., lines: _Optional[_Iterable[_Union[CodeLine, _Mapping]]] = ..., more: _Optional[int] = ...) -> None: ...

class SearchCodeResponse(_message.Message):
    __slots__ = ("hits", "files", "next_page_token", "indexed_at", "catching_up")
    HITS_FIELD_NUMBER: _ClassVar[int]
    FILES_FIELD_NUMBER: _ClassVar[int]
    NEXT_PAGE_TOKEN_FIELD_NUMBER: _ClassVar[int]
    INDEXED_AT_FIELD_NUMBER: _ClassVar[int]
    CATCHING_UP_FIELD_NUMBER: _ClassVar[int]
    hits: _containers.RepeatedCompositeFieldContainer[CodeHit]
    files: int
    next_page_token: str
    indexed_at: bytes
    catching_up: bool
    def __init__(self, hits: _Optional[_Iterable[_Union[CodeHit, _Mapping]]] = ..., files: _Optional[int] = ..., next_page_token: _Optional[str] = ..., indexed_at: _Optional[bytes] = ..., catching_up: _Optional[bool] = ...) -> None: ...

class FindSymbolRequest(_message.Message):
    __slots__ = ("name", "ref", "limit")
    NAME_FIELD_NUMBER: _ClassVar[int]
    REF_FIELD_NUMBER: _ClassVar[int]
    LIMIT_FIELD_NUMBER: _ClassVar[int]
    name: str
    ref: str
    limit: int
    def __init__(self, name: _Optional[str] = ..., ref: _Optional[str] = ..., limit: _Optional[int] = ...) -> None: ...

class SymbolHit(_message.Message):
    __slots__ = ("name", "kind", "path", "line", "text")
    NAME_FIELD_NUMBER: _ClassVar[int]
    KIND_FIELD_NUMBER: _ClassVar[int]
    PATH_FIELD_NUMBER: _ClassVar[int]
    LINE_FIELD_NUMBER: _ClassVar[int]
    TEXT_FIELD_NUMBER: _ClassVar[int]
    name: str
    kind: str
    path: str
    line: int
    text: str
    def __init__(self, name: _Optional[str] = ..., kind: _Optional[str] = ..., path: _Optional[str] = ..., line: _Optional[int] = ..., text: _Optional[str] = ...) -> None: ...

class FindSymbolResponse(_message.Message):
    __slots__ = ("symbols", "indexed_at", "catching_up")
    SYMBOLS_FIELD_NUMBER: _ClassVar[int]
    INDEXED_AT_FIELD_NUMBER: _ClassVar[int]
    CATCHING_UP_FIELD_NUMBER: _ClassVar[int]
    symbols: _containers.RepeatedCompositeFieldContainer[SymbolHit]
    indexed_at: bytes
    catching_up: bool
    def __init__(self, symbols: _Optional[_Iterable[_Union[SymbolHit, _Mapping]]] = ..., indexed_at: _Optional[bytes] = ..., catching_up: _Optional[bool] = ...) -> None: ...

class WhoAmIResponse(_message.Message):
    __slots__ = ("principal", "full_read", "read_prefixes")
    PRINCIPAL_FIELD_NUMBER: _ClassVar[int]
    FULL_READ_FIELD_NUMBER: _ClassVar[int]
    READ_PREFIXES_FIELD_NUMBER: _ClassVar[int]
    principal: str
    full_read: bool
    read_prefixes: _containers.RepeatedScalarFieldContainer[str]
    def __init__(self, principal: _Optional[str] = ..., full_read: _Optional[bool] = ..., read_prefixes: _Optional[_Iterable[str]] = ...) -> None: ...

class LockRequest(_message.Message):
    __slots__ = ("path",)
    PATH_FIELD_NUMBER: _ClassVar[int]
    path: str
    def __init__(self, path: _Optional[str] = ...) -> None: ...

class UnlockRequest(_message.Message):
    __slots__ = ("path", "force")
    PATH_FIELD_NUMBER: _ClassVar[int]
    FORCE_FIELD_NUMBER: _ClassVar[int]
    path: str
    force: bool
    def __init__(self, path: _Optional[str] = ..., force: _Optional[bool] = ...) -> None: ...

class ListLocksRequest(_message.Message):
    __slots__ = ("prefix",)
    PREFIX_FIELD_NUMBER: _ClassVar[int]
    prefix: str
    def __init__(self, prefix: _Optional[str] = ...) -> None: ...

class LockInfo(_message.Message):
    __slots__ = ("path", "owner", "locked_at_ms")
    PATH_FIELD_NUMBER: _ClassVar[int]
    OWNER_FIELD_NUMBER: _ClassVar[int]
    LOCKED_AT_MS_FIELD_NUMBER: _ClassVar[int]
    path: str
    owner: str
    locked_at_ms: int
    def __init__(self, path: _Optional[str] = ..., owner: _Optional[str] = ..., locked_at_ms: _Optional[int] = ...) -> None: ...

class LockList(_message.Message):
    __slots__ = ("locks",)
    LOCKS_FIELD_NUMBER: _ClassVar[int]
    locks: _containers.RepeatedCompositeFieldContainer[LockInfo]
    def __init__(self, locks: _Optional[_Iterable[_Union[LockInfo, _Mapping]]] = ...) -> None: ...
