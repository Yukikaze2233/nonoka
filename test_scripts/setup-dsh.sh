#!/usr/bin/env bash
set -euo pipefail

# 把 Nonoka 的 DSH 集成安装到 ~/.dsh：
#   1. 复制 agent preset 到 ~/.dsh/.agent-presets/nonoka
#   2. 把 mcp-nonoka 块注入 profiles/<profile>/cordis.patch.yml
#   3. 更新 profile package.json，让 DSH 识别 nonoka preset？不需要，
#      因为 DSH 会自动扫描 .agent-presets。
#
# 用法：scripts/setup-dsh.sh [profile] [nonoka-bin]
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${1:-web}"
NONOKA_BIN="${2:-$ROOT/target/debug/nonoka}"
DSH_HOME="${DSH_HOME:-$HOME/.dsh}"
PRESET_SRC="$ROOT/dsh/agent-presets/nonoka"
PRESET_DST="$DSH_HOME/.agent-presets/nonoka"
PATCH_FILE="$DSH_HOME/profiles/$PROFILE/cordis.patch.yml"

if [[ ! -x "$NONOKA_BIN" ]]; then
  echo "error: nonoka binary not found: $NONOKA_BIN" >&2
  echo "build it first: cargo build" >&2
  exit 1
fi

mkdir -p "$(dirname "$PRESET_DST")"
cp -a "$PRESET_SRC" "$(dirname "$PRESET_DST")"
echo "[setup-dsh] preset installed: $PRESET_DST"

mkdir -p "$(dirname "$PATCH_FILE")"
if [[ ! -f "$PATCH_FILE" ]]; then
  echo "[]" > "$PATCH_FILE"
fi

BEGIN="# === nonoka MCP BEGIN ==="
END="# === nonoka MCP END ==="
BLOCK="$BEGIN
- insert:
    - id: mcp-nonoka
      name: '@deepseek-ai/dsh-mcp-client'
      config:
        serverName: nonoka
        transport: stdio
        command: '$NONOKA_BIN'
        args:
          - 'mcp-serve'
$END"

python3 - "$PATCH_FILE" "$BEGIN" "$END" "$BLOCK" <<'PY'
import pathlib, sys
patch_file, begin, end, block = sys.argv[1:]
path = pathlib.Path(patch_file)
text = path.read_text()
if begin in text and end in text:
    import re
    text = re.sub(
        r'[^\n]*# === nonoka MCP BEGIN ===[\s\S]*?# === nonoka MCP END ===[^\n]*',
        block.strip(),
        text,
    )
else:
    # cordis.patch.yml 是单 YAML 文档：追加块序列前必须移除占位符
    # []，否则「注释 + []」会和新块解析成两个文档，DSH 启动会失败。
    text = text.replace('[]', '')
    if not text.strip():
        text = block + '\n'
    else:
        text = text.rstrip() + '\n\n' + block + '\n'
path.write_text(text)
PY
echo "[setup-dsh] MCP block installed in $PATCH_FILE"

echo "[setup-dsh] done. Restart DSH (or reload the web profile) to load the nonoka preset and MCP tools."
