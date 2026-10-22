#!/usr/bin/env bash
# Regenerate gRPC/protobuf stubs from ../../crates/proto/proto (run after protocol changes).
set -euo pipefail
cd "$(dirname "$0")"
OUT=src/grovedock/_gen
uv run --group dev python -m grpc_tools.protoc -I ../../crates/proto/proto \
  --python_out=$OUT --grpc_python_out=$OUT --pyi_out=$OUT \
  gd.proto gd_objects.proto
# grpc_tools emits absolute imports; make them package-relative.
sed -i.bak 's/^import gd_pb2 as/from . import gd_pb2 as/' $OUT/gd_pb2_grpc.py && rm $OUT/*.bak
rm -f $OUT/gd_objects_pb2_grpc.py  # message-only file, no services
