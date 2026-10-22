#!/usr/bin/env bash
# Reproducible git vs Grovedock benchmark on a real repository (default: git/git).
#
#   bench/run.sh                 # uses bench/work/git.git (cloned if missing)
#   REPO_URL=https://github.com/<org>/<repo>.git bench/run.sh
#
# Requires: hyperfine, git, python3, release builds (cargo build --release).
# Both systems run locally: git over file:// (real pack protocol), Grovedock over loopback gRPC.
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$ROOT/bench/work"
OUT="$ROOT/bench/results"
BIN="$ROOT/target/release"
REPO_URL="${REPO_URL:-https://github.com/git/git.git}"
RUNS="${RUNS:-5}"
PORT="${PORT:-7420}"
GD_URL="http://127.0.0.1:$PORT"
SPARSE_DIR="${SPARSE_DIR:-Documentation}"
FILE="${FILE:-Makefile}"
LOG_PATH="${LOG_PATH:-builtin/add.c}"

mkdir -p "$WORK" "$OUT"
cd "$WORK"
export PATH="$BIN:$PATH" GD_SERVER="$GD_URL"
export GIT_AUTHOR_NAME=bench GIT_AUTHOR_EMAIL=bench@example.com GIT_COMMITTER_NAME=bench GIT_COMMITTER_EMAIL=bench@example.com

[ -d git.git ] || git clone -q --bare "$REPO_URL" git.git
git -C git.git config uploadpack.allowFilter true
git -C git.git config uploadpack.allowAnySHA1InWant true
# hyperfine splits commands on whitespace; reach the corpus through a space-free symlink.
LINK="$(mktemp -d)/w" && ln -s "$WORK" "$LINK"
GIT_URL="file://$LINK/git.git"

# --- Grovedock server (SQLite, release build) with the full history imported ---------------
if ! gd refs >/dev/null 2>&1; then
  mkdir -p server
  grovedockd server --listen "127.0.0.1:$PORT" --metadata "sqlite://$WORK/server/meta.db" >server/log.txt 2>&1 &
  echo $! > server/pid
  for _ in $(seq 1 50); do gd refs >/dev/null 2>&1 && break; sleep 0.2; done
fi
# Idempotent: unchanged history re-imports without uploading anything. --force resets
# `main` to the git tip, discarding commits made by previous benchmark runs.
echo "== importing $(git -C git.git rev-list --count HEAD) commits into Grovedock"
/usr/bin/time -p gd import-git git.git --force 2> "$OUT/import-time.txt"

{
  echo "# git vs Grovedock: $(basename "$REPO_URL" .git)"
  echo
  echo "* Machine: $(sysctl -n machdep.cpu.brand_string 2>/dev/null || uname -m), $(sysctl -n hw.ncpu 2>/dev/null || nproc) cores"
  echo "* git $(git --version | awk '{print $3}'), gd $(gd --version | awk '{print $2}')"
  echo "* History: $(git -C git.git rev-list --count HEAD) commits, $(git -C git.git ls-tree -r HEAD | wc -l | tr -d ' ') files at HEAD"
  echo "* Each row: median of $RUNS runs (hyperfine), both systems local (git over file://, Grovedock over loopback gRPC)"
  echo
} > "$OUT/RESULTS.md"

hf() { # label, git command, gd command, [prepare]
  local name="$1" g="$2" s="$3" prep="${4:-true}"
  local id; id="$(echo "$name" | tr -c 'A-Za-z0-9\n' '-' | tr -s '-')"
  hyperfine -N --style basic --warmup 1 --runs "$RUNS" --prepare "$prep" \
    --export-json "$OUT/$id.json" -n git "$g" -n gd "$s" >/dev/null
  python3 - "$OUT/$id.json" "$name" >> "$OUT/RESULTS.md" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))["results"]
# hyperfine 2.x nests wall-clock stats under summary; 1.x has a top-level median.
med = lambda r: r["summary"]["time_wall_clock"]["median"] if "summary" in r else r["median"]
g, s = med(d[0]), med(d[1])
ratio = f"{g/s:.1f}x faster" if s < g else f"{s/g:.1f}x slower"
print(f"| {sys.argv[2]} | {g*1000:,.0f} ms | {s*1000:,.0f} ms | {ratio} |")
PY
}

echo "| Scenario | git | Grovedock | Grovedock vs git |" >> "$OUT/RESULTS.md"
echo "|---|---:|---:|---|" >> "$OUT/RESULTS.md"

hf "full-clone" \
  "git clone -q $GIT_URL g-full" \
  "gd clone $GD_URL s-full" \
  "rm -rf g-full s-full"

hf "sparse-clone ($SPARSE_DIR/)" \
  "bash -c 'git clone -q --filter=blob:none --sparse $GIT_URL g-sparse && git -C g-sparse sparse-checkout set $SPARSE_DIR'" \
  "gd clone $GD_URL s-sparse --path $SPARSE_DIR" \
  "rm -rf g-sparse s-sparse"

hf "read one file, no prior clone (CI)" \
  "bash -c 'git clone -q --depth 1 --filter=blob:none --no-checkout $GIT_URL g-ci && git -C g-ci show HEAD:$FILE >/dev/null'" \
  "gd cat main $FILE" \
  "rm -rf g-ci"

rm -rf g s && git clone -q "$GIT_URL" g && gd clone "$GD_URL" s >/dev/null
hf "status (clean, $(git -C g ls-files | wc -l | tr -d ' ') files)" \
  "git -C g status --porcelain" \
  "bash -c 'cd s && gd status'"

hf "log -n 50 -- $LOG_PATH" \
  "git -C g log -n 50 --first-parent --oneline -- $LOG_PATH" \
  "gd log $LOG_PATH -n 50 --oneline"

hf "blame $FILE" \
  "git -C g blame --first-parent -s $FILE" \
  "gd blame $FILE"

# Commit + publish a one-line change. git pushes to its own bare remote.
rm -rf push.git && git clone -q --bare "$GIT_URL" push.git && git -C g remote set-url origin "$LINK/push.git"
hf "commit + publish 1 file" \
  "bash -c 'git -C g commit -qam bench && git -C g push -q origin HEAD:master 2>/dev/null || git -C g push -q origin HEAD:main'" \
  "bash -c 'cd s && gd commit -m bench >/dev/null && gd land >/dev/null'" \
  "bash -c 'echo \$RANDOM >> g/README.md; echo \$RANDOM >> s/README.md'"

{
  echo
  echo "## Footprint after full clone"
  echo
  echo "| | git | Grovedock |"
  echo "|---|---:|---:|"
  echo "| Client metadata | $(du -sh g/.git | cut -f1) (\`.git\`, full history) | $(du -sh s/.gd | cut -f1) (\`.gd\`, index only) |"
  echo "| Server storage | $(du -sh git.git | cut -f1) (delta-compressed pack) | $(du -sh server | cut -f1) (per-object zstd, no deltas) |"
  echo
  echo "Full-history import: $(awk '/real/{print $2}' "$OUT/first-import-time.txt" 2>/dev/null || echo n/a) s (first run); re-import of unchanged history: $(awk '/real/{print $2}' "$OUT/import-time.txt") s (no uploads)"
} >> "$OUT/RESULTS.md"

python3 "$ROOT/bench/concurrent_land.py" --git-url "$GIT_URL" --gd-url "$GD_URL" --work "$WORK" >> "$OUT/RESULTS.md"

cat "$OUT/RESULTS.md"
