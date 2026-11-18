#!/usr/bin/env bash
# LobsterAI 指纹修复插件 · 安装钩子（install hook）。
#
# ── 这个脚本做什么 ────────────────────────────────────────────
# 把本仓库（打过 LobsterAI 官方指纹修复的 agent2api-providers 插件）装进
# Agent2API 的数据目录，让网关下一次启动就用它：
#
#   1. 校验输入（二进制存在、是 ELF、可执行；pack.json 可解析）
#   2. 以**本仓库自带**的 pack.json / assets / ui / licenses 为源，在数据目录里
#      铺出新版本包目录（干净检出也能装：不依赖已装的官方包做模板）
#   3. 改 pack.json：版本号提到目标值、**摘掉 signature 段**
#      （本机自编二进制没有官方私钥签名；网关只对带签名段的包做 ed25519 验签，
#       没有签名段就按「本地包」处理，且不会被官方自动更新覆盖）
#   4. 用本仓库的二进制覆盖入口 bin/agent2api-providers
#   5. 复核待装包（入口可执行、pack.json 可解析、lobsterai 在 manifest 清单里；
#      这一步在动注册表之前做，失败时注册表还是原样）
#   6. 备份 plugins.json（plugins.json.bak.<时间戳>，回滚即恢复它）
#   7. 改 plugins.json：版本、入口、sha256、signed=false、installed_at、provider 清单
#      （逐家启停沿用旧包，新出现的 provider 补默认值；lobsterai 必须在列）
#   8. 终核：注册表 sha256 与实际文件一致、lobsterai 在清单里（停用只警告，
#      不改启停）；失败时自动恢复备份
#
# ── 用法（在仓库根目录运行）──────────────────────────────────
#   ./tools/install-lobsterai-fix.sh                      # 装到探测到的数据目录，版本取 pack.json 的 version
#   ./tools/install-lobsterai-fix.sh --home /data         # 显式指定（等价于 AGENT2API_PROXY_HOME）
#   ./tools/install-lobsterai-fix.sh --dry-run            # 只打印计划，不动文件
#   ./tools/install-lobsterai-fix.sh --binary PATH        # 用指定二进制（默认本仓库 dist/agent2api-providers）
#   ./tools/install-lobsterai-fix.sh --version V          # 目标版本号（默认取 pack.json 的 version）
#   ./tools/install-lobsterai-fix.sh --rollback           # 用最近一次备份恢复 plugins.json（并删掉新版本目录）
#
# ── 硬约束 ──────────────────────────────────────────────────
# 不联网、不发布、不碰别家 provider 的实现，只动 agent2api-providers 这一包的
# 版本目录与注册表条目。任何一步失败都停在原地并打印已做了什么。
# **不需要任何密钥 / 令牌**：脚本只读本目录的 pack.json 与二进制，凭证由使用者
# 在网关面板里填，脚本不读、不写、不打印凭证。
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# 脚本在 tools/ 里，pack.json / dist / assets / ui / licenses 都在仓库根目录，
# 所以路径一律按仓库根目录算（旧代码用 $HERE 拼，清单与二进制都找不到，
# 2026-11-18 实测：直接报「清单不存在：tools/pack.json」）。
ROOT="$(cd "$HERE/.." && pwd)"
PACK_ID="agent2api-providers"
MANIFEST="$ROOT/pack.json"
BINARY="$ROOT/dist/agent2api-providers"
HOME_DIR=""
DRY_RUN=0
ROLLBACK=0
TARGET_VERSION=""

while [ $# -gt 0 ]; do
  case "$1" in
    --home) HOME_DIR="${2:?--home 需要目录}"; shift 2 ;;
    --version) TARGET_VERSION="${2:?--version 需要版本号}"; shift 2 ;;
    --binary) BINARY="${2:?--binary 需要路径}"; shift 2 ;;
    --dry-run) DRY_RUN=1; shift ;;
    --rollback) ROLLBACK=1; shift ;;
    -h|--help) sed -n '2,32p' "$0"; exit 0 ;;
    *) echo "未知参数：$1" >&2; exit 2 ;;
  esac
done

say() { printf '%s\n' "$*"; }
die() { printf '错误：%s\n' "$*" >&2; exit 1; }

# ── 0. 目标版本（默认取 manifest 的 version）──────────────────
if [ -z "$TARGET_VERSION" ]; then
  TARGET_VERSION="$(python3 -c "import json,sys;print(json.load(open(sys.argv[1]))['version'])" "$MANIFEST")"
fi

# 版本号只允许进版本目录：含 / 或 .. 会让 TARGET_DIR 走出插件目录
# （回滚分支的 rm -rf 用的就是同一份 TARGET_DIR），所以在用它拼任何
# 路径之前先拦下来。
case "$TARGET_VERSION" in
  ""|*"/"*|*".."*) die "版本号不合法：$TARGET_VERSION（不能空、不能含 / 或 ..）" ;;
esac

# ── 1. 找数据目录（与网关同一口径：AGENT2API_PROXY_HOME → WORKBUDDY_PROXY_HOME → ~/.agent2api）──
if [ -z "$HOME_DIR" ]; then
  if [ -n "${AGENT2API_PROXY_HOME:-}" ]; then
    HOME_DIR="$AGENT2API_PROXY_HOME"
  elif [ -n "${WORKBUDDY_PROXY_HOME:-}" ]; then
    HOME_DIR="$WORKBUDDY_PROXY_HOME"
  elif [ -d "$HERE/../../data/plugins" ]; then
    # 本仓库的部署形态：docker-compose 把 ./data 挂成 /data
    HOME_DIR="$(cd "$HERE/../../data" && pwd)"
  else
    HOME_DIR="$HOME/.agent2api"
  fi
fi
PLUGINS_DIR="$HOME_DIR/plugins"
REGISTRY="$PLUGINS_DIR/plugins.json"
TARGET_DIR="$PLUGINS_DIR/$PACK_ID/$TARGET_VERSION"

say "== LobsterAI 指纹修复插件 =="
say "数据目录      : $HOME_DIR"
say "目标版本      : $TARGET_VERSION"
say "包目录        : $TARGET_DIR"
say "二进制        : $BINARY"

# ── 2. 回滚分支 ──────────────────────────────────────────────
if [ "$ROLLBACK" -eq 1 ]; then
  BACKUP="$(ls -1t "$PLUGINS_DIR"/plugins.json.bak.* 2>/dev/null | head -1 || true)"
  if [ -z "$BACKUP" ]; then
    # 干净检出可能没有备份：直接删掉本包的目标目录即可
    if [ -d "$TARGET_DIR" ]; then
      if [ "$DRY_RUN" -eq 1 ]; then say "(dry-run，未改动)"; exit 0; fi
      rm -rf "$TARGET_DIR"
      say "已删除包目录：$TARGET_DIR（无 plugins.json 备份可恢复）"
    else
      say "没有可回滚的内容（无备份、无目标目录）"
    fi
    exit 0
  fi
  say "将用备份恢复注册表：$BACKUP"
  if [ "$DRY_RUN" -eq 1 ]; then say "(dry-run，未改动)"; exit 0; fi
  mkdir -p "$PLUGINS_DIR"
  cp -f "$BACKUP" "$REGISTRY"
  if [ -d "$TARGET_DIR" ]; then
    rm -rf "$TARGET_DIR"
    say "已删除包目录：$TARGET_DIR"
  fi
  say "回滚完成。重启网关后生效。"
  exit 0
fi

# ── 3. 校验输入 ──────────────────────────────────────────────
[ -f "$MANIFEST" ] || die "清单不存在：$MANIFEST"
python3 -c "import json,sys;json.load(open(sys.argv[1]))" "$MANIFEST" || die "pack.json 不可解析：$MANIFEST"
[ -f "$BINARY" ] || die "二进制不存在：$BINARY（先 ./build-fx.sh，或用 --binary 指定）"
file "$BINARY" | grep -q ELF || die "二进制不是 ELF 可执行文件：$BINARY"
chmod +x "$BINARY"

# 入口相对路径：优先按本机 target 从 manifest 的 entry 字典取
HOST_TRIPLE="$( (rustc -vV 2>/dev/null | sed -n 's/^host: //p') || true)"
if [ -z "$HOST_TRIPLE" ]; then
  case "$(uname -s)/$(uname -m)" in
    Linux/x86_64) HOST_TRIPLE="x86_64-unknown-linux-gnu" ;;
    Linux/aarch64|Linux/arm64) HOST_TRIPLE="aarch64-unknown-linux-gnu" ;;
    Darwin/x86_64) HOST_TRIPLE="x86_64-apple-darwin" ;;
    Darwin/arm64) HOST_TRIPLE="aarch64-apple-darwin" ;;
    *) HOST_TRIPLE="x86_64-unknown-linux-gnu" ;;
  esac
fi
ENTRY_REL="$(python3 - "$MANIFEST" "$HOST_TRIPLE" <<'PY'
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
target = sys.argv[2]
entry = manifest.get("entry")
if isinstance(entry, dict):
    entry = entry.get(target) or entry.get("x86_64-unknown-linux-gnu")
print(entry or "bin/agent2api-providers")
PY
)"
say "入口          : $ENTRY_REL"

if [ "$DRY_RUN" -eq 1 ]; then
  say "计划："
  say "  1) 铺包目录 $TARGET_DIR（pack.json / assets / ui / licenses 取自本仓库）"
  say "  2) pack.json：version=$TARGET_VERSION，删除 signature 段"
  say "  3) 用 $BINARY 覆盖 $TARGET_DIR/$ENTRY_REL"
  say "  4) 复核待装包（入口可执行、pack.json 可解析、lobsterai 在 manifest 清单里；失败时不改注册表）"
  say "  5) 备份 $REGISTRY → plugins.json.bak.<时间戳>（无则新建）"
  say "  6) 注册表条目：version=$TARGET_VERSION / sha256=<新二进制> / signed=false"
  say "  7) 终核：注册表 sha256 与实际文件一致、lobsterai 在清单里（失败时自动恢复备份）"
  say "(dry-run，未改动)"
  exit 0
fi

# ── 4. 铺包目录（自包含：不依赖已装的官方包做模板）──────────
mkdir -p "$TARGET_DIR"
# pack.json 单独处理（要改版本 + 摘签名），其余静态资源原样复制
if [ -d "$ROOT/assets" ]; then cp -a "$ROOT/assets" "$TARGET_DIR/assets"; fi
if [ -d "$ROOT/ui" ]; then cp -a "$ROOT/ui" "$TARGET_DIR/ui"; fi
if [ -d "$ROOT/licenses" ]; then cp -a "$ROOT/licenses" "$TARGET_DIR/licenses"; fi

# ── 5. pack.json：提版本 + 摘签名 ────────────────────────────
cp -f "$MANIFEST" "$TARGET_DIR/pack.json"
python3 - "$TARGET_DIR/pack.json" "$TARGET_VERSION" <<'PY'
import json, sys
path, version = sys.argv[1], sys.argv[2]
manifest = json.load(open(path, encoding="utf-8"))
manifest["version"] = version
# 签名段绑的是官方二进制的 sha256；换成本地二进制后必须摘掉，
# 否则网关验签必然失败（verify_signature → Invalid）。
manifest.pop("signature", None)
json.dump(manifest, open(path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("pack.json 已更新：version=%s，signature 已摘除" % version)
PY

# ── 6. 放二进制 ──────────────────────────────────────────────
mkdir -p "$TARGET_DIR/$(dirname "$ENTRY_REL")"
cp -f "$BINARY" "$TARGET_DIR/$ENTRY_REL"
chmod +x "$TARGET_DIR/$ENTRY_REL"

# ── 7. 复核待装包（在动注册表之前做：这一步失败时注册表还是原样）──
say "复核待装包 …"
[ -x "$TARGET_DIR/$ENTRY_REL" ] || die "复核失败：入口不可执行：$TARGET_DIR/$ENTRY_REL"
python3 -c "import json,sys;json.load(open(sys.argv[1]))" "$TARGET_DIR/pack.json" || die "复核失败：pack.json 不可解析"
python3 - "$TARGET_DIR/pack.json" <<'PY' || die "复核失败：lobsterai 不在 manifest 的 provider 清单里"
import json, sys
manifest = json.load(open(sys.argv[1], encoding="utf-8"))
ids = [item.get("id") for item in manifest.get("providers", []) or []]
assert "lobsterai" in ids, "lobsterai missing from manifest providers"
print("lobsterai 已在 manifest 的 provider 清单里")
PY

# ── 8. 备份注册表 ────────────────────────────────────────────
REGISTRY_BACKUP=""
REGISTRY_EXISTED=0
mkdir -p "$PLUGINS_DIR"
if [ -f "$REGISTRY" ]; then
  REGISTRY_EXISTED=1
  STAMP="$(date +%Y%m%d-%H%M%S)"
  REGISTRY_BACKUP="$REGISTRY.bak.$STAMP"
  cp -f "$REGISTRY" "$REGISTRY_BACKUP"
  say "已备份注册表 → $REGISTRY_BACKUP"
else
  say "（注册表不存在：将新建 $REGISTRY）"
fi

# 终核失败时把注册表恢复到安装前（有备份就恢复备份，没备份就删掉新建的注册表）。
restore_registry() {
  if [ "$REGISTRY_EXISTED" -eq 1 ] && [ -n "$REGISTRY_BACKUP" ] && [ -f "$REGISTRY_BACKUP" ]; then
    cp -f "$REGISTRY_BACKUP" "$REGISTRY"
    say "已恢复注册表备份：$REGISTRY_BACKUP"
  elif [ "$REGISTRY_EXISTED" -eq 0 ]; then
    rm -f "$REGISTRY"
    say "已删除新建的注册表（恢复到安装前无注册表的状态）"
  fi
}

# ── 9. 改注册表条目 ──────────────────────────────────────────
python3 - "$REGISTRY" "$TARGET_DIR" "$TARGET_VERSION" "$ENTRY_REL" <<'PY'
import hashlib, json, os, sys, time

registry_path, target_dir, version, entry = sys.argv[1:5]

def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for block in iter(lambda: handle.read(1 << 20), b""):
            digest.update(block)
    return digest.hexdigest()

manifest = json.load(open("%s/pack.json" % target_dir, encoding="utf-8"))
if os.path.exists(registry_path):
    registry = json.load(open(registry_path, encoding="utf-8"))
else:
    registry = {"packs": []}
packs = registry.setdefault("packs", [])
pack = next((item for item in packs if item.get("id") == "agent2api-providers"), None)
if pack is None:
    pack = {"id": "agent2api-providers"}
    packs.append(pack)

binary_hash = sha256_file("%s/%s" % (target_dir, entry))

# 逐家启停沿用旧包；manifest 里新出现的 provider 补默认开启状态
previous = pack.get("providers", {}) or {}
providers = {}
for item in manifest.get("providers", []) or []:
    pid = item.get("id")
    if not pid:
        continue
    providers[pid] = previous.get(pid, bool(item.get("default_enabled", True)))

pack["version"] = version
pack["entry"] = entry
pack["sha256"] = binary_hash
# 本地自编二进制没有官方私钥签名：显式标 false，网关因此不会自动用官方包覆盖它
pack["signed"] = False
pack["installed_at"] = int(time.time() * 1000)
pack["providers"] = providers
pack["manifest_providers"] = manifest.get("providers", []) or []

json.dump(registry, open(registry_path, "w", encoding="utf-8"), ensure_ascii=False, indent=2)
print("注册表已更新：version=%s sha256=%s signed=false providers=%d"
      % (version, binary_hash[:16] + "…", len(providers)))
PY

# ── 10. 终核（只核对注册表与已装文件一致；失败时自动恢复备份）──
# 入口可执行 / pack.json 可解析已在 §7 动注册表之前验过，这里不再重复。
say "复核注册表 …"
WANT="$(python3 -c "import hashlib,sys;print(hashlib.sha256(open(sys.argv[1],'rb').read()).hexdigest())" "$TARGET_DIR/$ENTRY_REL")"
GOT="$(python3 -c "import json,sys;r=json.load(open(sys.argv[1]));print([p['sha256'] for p in r['packs'] if p['id']=='agent2api-providers'][0])" "$REGISTRY")"
[ "$WANT" = "$GOT" ] || { restore_registry; die "复核失败：注册表 sha256 与实际文件不一致（已恢复备份）"; }
# lobsterai 必须在 provider 清单里；但它在面板里事先停用时只警告，
# 不改启停、不算安装失败（启停是面板的状态，不是安装包的问题）。
python3 - "$REGISTRY" <<'PY' || { restore_registry; die "复核失败：lobsterai 不在 provider 清单里（已恢复备份）"; }
import json, sys
registry = json.load(open(sys.argv[1], encoding="utf-8"))
pack = next(p for p in registry["packs"] if p["id"] == "agent2api-providers")
assert "lobsterai" in pack["providers"], "lobsterai missing from providers"
if pack["providers"]["lobsterai"] is not True:
    print("注意：lobsterai 在面板里处于停用状态，本次安装不改动它的启停；如需启用请在面板里打开")
else:
    print("lobsterai 已在 provider 清单且为启用状态")
PY
say "复核通过：sha256 一致，lobsterai 在清单里"

say ""
say "✅ 安装完成"
say "  包目录  : $TARGET_DIR"
say "  入口    : $ENTRY_REL"
say "  sha256  : $WANT"
say "  回滚    : $0 --home $HOME_DIR --rollback"
say "  重启网关后生效（容器：docker compose restart）"
