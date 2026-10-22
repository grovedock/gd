#!/usr/bin/env bash
# Regenerate Go stubs from ../../crates/proto/proto (run after protocol changes).
set -euo pipefail
cd "$(dirname "$0")"
M=github.com/grovedock/gd/sdk/go/v2
protoc -I ../../crates/proto/proto \
  --go_out=. --go_opt=module=$M \
  --go-grpc_out=. --go-grpc_opt=module=$M \
  --go_opt=Mgd.proto=$M/gdpb --go-grpc_opt=Mgd.proto=$M/gdpb \
  --go_opt=Mgd_objects.proto=$M/objectspb --go-grpc_opt=Mgd_objects.proto=$M/objectspb \
  gd.proto gd_objects.proto
