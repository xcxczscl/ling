#!/bin/sh
# package.sh — resources-manager 发行包组装（M6）
#
# 用法：
#   scripts/package.sh                 # 默认 x86_64-unknown-linux-musl
#   TARGET=x86_64-unknown-linux-gnu scripts/package.sh
#
# 产物：
#   dist/release/              组装目录（可直接拷贝使用）
#   dist/resources-manager-<version>-<target>.tar.gz
#
# 依赖：
#   - Rust toolchain（musl 目标建议 `rustup target add x86_64-unknown-linux-musl`）
#   - ling 与 resources-manager 两个仓库并列放在同一父目录
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
RM_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
LING_ROOT=$(CDPATH= cd -- "$RM_ROOT/../ling" && pwd)
VERSION=$(grep -m1 '^version' "$RM_ROOT/Cargo.toml" | sed 's/.*"\(.*\)"/\1/')
# TARGET 为空 = 主机默认目标（Alpine musl）；显式设置（如 x86_64-unknown-linux-gnu）则用 --target。
TARGET="${TARGET:-}"
OUT="$RM_ROOT/dist/release"
TARBALL="$RM_ROOT/dist/resources-manager-${VERSION}-${TARGET:-host}.tar.gz"

echo "==> 版本 $VERSION  目标 ${TARGET:-host}"
echo "==> resources-manager: $RM_ROOT"
echo "==> ling:              $LING_ROOT"

# ---------------------------------------------------------------- build
# TARGET 为空 = 主机默认目标（Alpine 上即 musl）；显式设置则用 --target。
if [ -n "$TARGET" ]; then
  echo "==> 构建 ling（release，target=$TARGET）"
  ( cd "$LING_ROOT" && cargo build --release --target "$TARGET" \
      -p ling-cli -p core-ui -p core-settings -p core-plugin-manager -p core-job -p core-log )
  echo "==> 构建 resources-manager（release，target=$TARGET）"
  ( cd "$RM_ROOT" && cargo build --release --target "$TARGET" \
      -p rmgr -p rm-plugin )
  LING_BIN="$LING_ROOT/target/$TARGET/release"
  RM_BIN="$RM_ROOT/target/$TARGET/release"
else
  echo "==> 构建 ling（release，主机默认目标）"
  ( cd "$LING_ROOT" && cargo build --release \
      -p ling-cli -p core-ui -p core-settings -p core-plugin-manager -p core-job -p core-log )
  echo "==> 构建 resources-manager（release，主机默认目标）"
  ( cd "$RM_ROOT" && cargo build --release -p rmgr -p rm-plugin )
  LING_BIN="$LING_ROOT/target/release"
  RM_BIN="$RM_ROOT/target/release"
fi

# ---------------------------------------------------------------- assemble
echo "==> 组装 $OUT"
rm -rf "$OUT"
mkdir -p "$OUT/bin" "$OUT/builtin-plugins" "$OUT/plugins" \
         "$OUT/packaging/systemd" "$OUT/packaging/windows"

# 二进制
cp "$LING_BIN/ling"            "$OUT/bin/ling"
cp "$RM_BIN/rmgr"              "$OUT/bin/rmgr"
chmod 755 "$OUT/bin/ling" "$OUT/bin/rmgr"

# 内置插件（随包分发）
for id in core-ui core-settings core-plugin-manager core-job core-log; do
  mkdir -p "$OUT/builtin-plugins/$id"
  cp "$LING_ROOT/builtin-plugins/$id/plugin.toml" "$OUT/builtin-plugins/$id/"
  cp "$LING_BIN/lib${id//-/_}.so" "$OUT/builtin-plugins/$id/$id.so"
done

# 资源管理插件
mkdir -p "$OUT/plugins/resources-manager"
cp "$RM_ROOT/plugins/resources-manager/plugin.toml" "$OUT/plugins/resources-manager/"
cp "$RM_BIN/libresources_manager.so" "$OUT/plugins/resources-manager/resources-manager.so"

# 服务注册文件
cp "$RM_ROOT/packaging/systemd/resources-manager.service" "$OUT/packaging/systemd/"
cp "$RM_ROOT/packaging/windows/install-service.ps1"      "$OUT/packaging/windows/"
cp "$RM_ROOT/packaging/windows/uninstall-service.ps1"    "$OUT/packaging/windows/"
cp "$RM_ROOT/packaging/INSTALL.txt" "$OUT/"

# ---------------------------------------------------------------- tarball
echo "==> 打包 $TARBALL"
( cd "$RM_ROOT/dist" && tar czf "$(basename "$TARBALL")" release )

# ---------------------------------------------------------------- verify
echo "==> 校验产物"
fail=0
for f in \
  "$OUT/bin/ling" "$OUT/bin/rmgr" \
  "$OUT/builtin-plugins/core-ui/core-ui.so" \
  "$OUT/builtin-plugins/core-settings/core-settings.so" \
  "$OUT/builtin-plugins/core-plugin-manager/core-plugin-manager.so" \
  "$OUT/builtin-plugins/core-job/core-job.so" \
  "$OUT/builtin-plugins/core-log/core-log.so" \
  "$OUT/plugins/resources-manager/resources-manager.so" \
  "$OUT/plugins/resources-manager/plugin.toml" \
  "$OUT/packaging/systemd/resources-manager.service" \
  "$OUT/packaging/windows/install-service.ps1"
do
  if [ ! -f "$f" ]; then
    echo "  缺失: $f"
    fail=1
  fi
done
[ "$fail" -eq 0 ] || { echo "打包失败：产物不完整"; exit 1; }

echo "==> 完成"
echo "    目录: $OUT"
echo "    压缩包: $TARBALL"
ls -lh "$TARBALL"
