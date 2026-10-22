from google.protobuf.internal import containers as _containers
from google.protobuf.internal import enum_type_wrapper as _enum_type_wrapper
from google.protobuf import descriptor as _descriptor
from google.protobuf import message as _message
from collections.abc import Iterable as _Iterable, Mapping as _Mapping
from typing import ClassVar as _ClassVar, Optional as _Optional, Union as _Union

DESCRIPTOR: _descriptor.FileDescriptor

class Mode(int, metaclass=_enum_type_wrapper.EnumTypeWrapper):
    __slots__ = ()
    FILE: _ClassVar[Mode]
    EXEC: _ClassVar[Mode]
    SYMLINK: _ClassVar[Mode]
    DIR: _ClassVar[Mode]
FILE: Mode
EXEC: Mode
SYMLINK: Mode
DIR: Mode

class TreeEntry(_message.Message):
    __slots__ = ("name", "mode", "id")
    NAME_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    ID_FIELD_NUMBER: _ClassVar[int]
    name: str
    mode: Mode
    id: bytes
    def __init__(self, name: _Optional[str] = ..., mode: _Optional[_Union[Mode, str]] = ..., id: _Optional[bytes] = ...) -> None: ...

class Tree(_message.Message):
    __slots__ = ("entries",)
    ENTRIES_FIELD_NUMBER: _ClassVar[int]
    entries: _containers.RepeatedCompositeFieldContainer[TreeEntry]
    def __init__(self, entries: _Optional[_Iterable[_Union[TreeEntry, _Mapping]]] = ...) -> None: ...

class FileChange(_message.Message):
    __slots__ = ("path", "blob", "mode")
    PATH_FIELD_NUMBER: _ClassVar[int]
    BLOB_FIELD_NUMBER: _ClassVar[int]
    MODE_FIELD_NUMBER: _ClassVar[int]
    path: str
    blob: bytes
    mode: Mode
    def __init__(self, path: _Optional[str] = ..., blob: _Optional[bytes] = ..., mode: _Optional[_Union[Mode, str]] = ...) -> None: ...

class Changeset(_message.Message):
    __slots__ = ("parents", "changes", "author", "message", "timestamp_ms")
    PARENTS_FIELD_NUMBER: _ClassVar[int]
    CHANGES_FIELD_NUMBER: _ClassVar[int]
    AUTHOR_FIELD_NUMBER: _ClassVar[int]
    MESSAGE_FIELD_NUMBER: _ClassVar[int]
    TIMESTAMP_MS_FIELD_NUMBER: _ClassVar[int]
    parents: _containers.RepeatedScalarFieldContainer[bytes]
    changes: _containers.RepeatedCompositeFieldContainer[FileChange]
    author: str
    message: str
    timestamp_ms: int
    def __init__(self, parents: _Optional[_Iterable[bytes]] = ..., changes: _Optional[_Iterable[_Union[FileChange, _Mapping]]] = ..., author: _Optional[str] = ..., message: _Optional[str] = ..., timestamp_ms: _Optional[int] = ...) -> None: ...

class ChunkRef(_message.Message):
    __slots__ = ("id", "len")
    ID_FIELD_NUMBER: _ClassVar[int]
    LEN_FIELD_NUMBER: _ClassVar[int]
    id: bytes
    len: int
    def __init__(self, id: _Optional[bytes] = ..., len: _Optional[int] = ...) -> None: ...

class Manifest(_message.Message):
    __slots__ = ("size", "chunks")
    SIZE_FIELD_NUMBER: _ClassVar[int]
    CHUNKS_FIELD_NUMBER: _ClassVar[int]
    size: int
    chunks: _containers.RepeatedCompositeFieldContainer[ChunkRef]
    def __init__(self, size: _Optional[int] = ..., chunks: _Optional[_Iterable[_Union[ChunkRef, _Mapping]]] = ...) -> None: ...
