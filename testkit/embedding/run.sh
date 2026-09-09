#!/usr/bin/env bash
# 语义检索黑盒验收(09-05):在 NONOKA_HOME 沙箱里跑 `nonoka embed` 三件套、表情包
# 检索(关键词 vs 融合)、知识库语义检索、以及「运行库缺失 / 总开关关闭」两种退化。
# 用法: testkit/embedding/run.sh [nonoka 二进制路径]   默认 target/release/nonoka
set -u
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
NONOKA="${1:-$ROOT/target/release/nonoka}"
H="$HERE/home"
export NONOKA_HOME="$H" HOME="$H" XDG_RUNTIME_DIR="/tmp/mx-embed-$$"
unset XDG_CACHE_HOME XDG_CONFIG_HOME XDG_DATA_HOME XDG_STATE_HOME
mkdir -p "$H/config" "$H/data" "$XDG_RUNTIME_DIR"
export NONOKA_EMBEDDING_MODELS_DIR="$ROOT/assets/models"
# 沙箱 HOME 下没有 ~/.nonoka/lib;没装系统 onnxruntime 时用 ORT_LIB 指一个运行库。
ORT_LIB="${ORT_LIB:-/home/shorin/.nonoka/lib/libonnxruntime.so}"
if [[ ! -f /usr/lib/libonnxruntime.so && -f "$ORT_LIB" ]]; then
  export NONOKA_ONNXRUNTIME_LIB="$ORT_LIB"
fi

pass=0; fail=0
check() { # name, condition
  if eval "$2"; then echo "PASS  $1"; pass=$((pass+1)); else echo "FAIL  $1"; fail=$((fail+1)); fi
}

# 最小配置:默认值 + 一个假供应商,不需要任何 API。
cat > "$H/config/config.jsonc" <<'EOF'
{
  "providers": [{ "id": "stub", "display_name": "stub", "base_url": "http://127.0.0.1:9", "api_key": "x", "models": ["m"], "default_model": "m" }],
  "active_provider_models": [{ "provider_id": "stub", "model": "m" }]
}
EOF

# 表情库:复制真实库(只读取,不改动原库)。
LIB="$H/data/memes/nonoka"
SRC_MEMES="${MEMES_SRC:-/home/shorin/.nonoka/data/memes/nonoka}"
if [[ -d "$SRC_MEMES" && ! -d "$LIB" ]]; then
  mkdir -p "$LIB" && cp -r "$SRC_MEMES"/. "$LIB"/
fi

echo "== embed models"
"$NONOKA" embed models | tee "$HERE/out-models.txt"
check "models lists bundled model" 'grep -q "bge-small-zh-v1.5-int8" "$HERE/out-models.txt"'

echo "== embed status"
"$NONOKA" embed status | tee "$HERE/out-status.txt"
check "status: semantic available" 'grep -q "semantic search: available" "$HERE/out-status.txt"'
check "status: probe 512 dims" 'grep -q "probe: 512 dims" "$HERE/out-status.txt"'

echo "== meme search: keyword-only baseline (embedding disabled via env-less config toggle)"
python3 - "$H/config/config.jsonc" <<'EOF'
import json,sys
p=sys.argv[1]; c=json.load(open(p)); c["embedding"]={"enabled": False}; json.dump(c,open(p,"w"),ensure_ascii=False,indent=2)
EOF
"$NONOKA" __tool use_meme '{"action":"search","query":"朋友说错话想吐槽","limit":3}' | tee "$HERE/out-meme-keyword.txt"
python3 - "$H/config/config.jsonc" <<'EOF'
import json,sys
p=sys.argv[1]; c=json.load(open(p)); c.pop("embedding",None); json.dump(c,open(p,"w"),ensure_ascii=False,indent=2)
EOF
echo "== meme search: fused"
"$NONOKA" __tool use_meme '{"action":"search","query":"朋友说错话想吐槽","limit":3}' | tee "$HERE/out-meme-fused.txt"
check "fused meme search returned candidates" 'grep -q "candidate(s)" "$HERE/out-meme-fused.txt" && ! grep -q "0 candidate" "$HERE/out-meme-fused.txt"'
check "meme vector cache created" 'ls "$H"/cache/meme-embeddings/*.db >/dev/null 2>&1'

echo "== knowledge base"
mkdir -p "$HERE/kbsrc"
printf '# 显卡驱动\n\n安装 nvidia 驱动后黑屏,进 tty 把 nvidia 模块加进 mkinitcpio 并重建镜像。\n' > "$HERE/kbsrc/gpu.md"
printf '# 午饭\n\n楼下的麻辣烫和新开的炸鸡店评价都不错。\n' > "$HERE/kbsrc/lunch.md"
"$NONOKA" kb add "$HERE/kbsrc/gpu.md" >/dev/null && "$NONOKA" kb add "$HERE/kbsrc/lunch.md" >/dev/null
"$NONOKA" kb embed reindex | tee "$HERE/out-kb-reindex.txt"
"$NONOKA" kb search "N 卡装好之后开机屏幕不亮" | tee "$HERE/out-kb-search.txt"
check "kb semantic hit on paraphrase" 'grep -q "gpu.md" "$HERE/out-kb-search.txt"'

echo "== reindex"
"$NONOKA" embed reindex | tee "$HERE/out-reindex.txt"
check "reindex ran" 'grep -q "memes (nonoka): embedded" "$HERE/out-reindex.txt"'

echo "== degrade: runtime library missing"
NONOKA_ONNXRUNTIME_LIB=/nonexistent/libonnxruntime.so PATH_SAVE="$PATH" \
  env NONOKA_ONNXRUNTIME_LIB=/nonexistent/libonnxruntime.so "$NONOKA" embed status | tee "$HERE/out-status-nolib.txt" || true
check "nolib: status explains" 'grep -q "keyword search still works\|semantic search: unavailable" "$HERE/out-status-nolib.txt"'
env NONOKA_ONNXRUNTIME_LIB=/nonexistent/libonnxruntime.so "$NONOKA" __tool use_meme '{"action":"search","query":"滑稽","limit":3}' | tee "$HERE/out-meme-nolib.txt"
check "nolib: meme search still answers" 'grep -q "candidate(s)" "$HERE/out-meme-nolib.txt"'

echo "== degrade: embedding.enabled=false"
python3 - "$H/config/config.jsonc" <<'EOF'
import json,sys
p=sys.argv[1]; c=json.load(open(p)); c["embedding"]={"enabled": False}; json.dump(c,open(p,"w"),ensure_ascii=False,indent=2)
EOF
"$NONOKA" embed status | tee "$HERE/out-status-disabled.txt"
check "disabled: status says disabled" 'grep -q "disabled in config" "$HERE/out-status-disabled.txt"'

echo
echo "passed=$pass failed=$fail  (outputs in $HERE/out-*.txt)"
rm -rf "$XDG_RUNTIME_DIR"
[[ $fail -eq 0 ]]
