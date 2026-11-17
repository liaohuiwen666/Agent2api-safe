#!/bin/bash
# Agent2api-safe 构建脚本：编译 providers 插件包 → dist/agent2api-providers
#
# 依赖：curl、gcc/ld、python3、（可选但强烈建议）mold
# 说明：
#   - 在 Linux 宿主机直接编译；产物 glibc 版本 = 构建宿主 glibc，须 ≤ 运行容器
#     （官方容器基于 Debian 12 / glibc 2.36）
#   - 默认关 LTO、codegen-units=8、-j2，适配 2 核 / <2G 内存的机器
#   - 国内网络默认走 USTC（rustup）与 rsproxy（crates）镜像，可按需替换
set -euo pipefail
cd "$(dirname "$0")"

export PATH="$HOME/.cargo/bin:$PATH"
export RUSTUP_DIST_SERVER="${RUSTUP_DIST_SERVER:-https://mirrors.ustc.edu.cn/rust-static}"
export RUSTUP_UPDATE_ROOT="${RUSTUP_UPDATE_ROOT:-https://mirrors.ustc.edu.cn/rust-static/rustup}"

if ! command -v cargo >/dev/null 2>&1; then
  rm -f rustup-init
  ok=0
  for u in \
    "$RUSTUP_DIST_SERVER/rustup/dist/x86_64-unknown-linux-gnu/rustup-init" \
    "https://rsproxy.cn/rustup/dist/x86_64-unknown-linux-gnu/rustup-init"; do
    echo "try: $u"
    if curl -fsSL --max-time 180 --retry 3 "$u" -o rustup-init; then
      sz=$(stat -c%s rustup-init)
      echo "downloaded bytes: $sz"
      if [ "$sz" -gt 15000000 ] && chmod +x rustup-init && ./rustup-init --version >/dev/null 2>&1; then
        ok=1; break
      fi
    fi
    rm -f rustup-init
  done
  [ "$ok" = 1 ] || { echo "RUSTUP_DOWNLOAD_FAILED"; exit 1; }
  ./rustup-init -y --profile minimal --default-toolchain stable
  rm -f rustup-init
fi

if [ ! -f "$HOME/.cargo/config.toml" ]; then
  mkdir -p "$HOME/.cargo"
  cat > "$HOME/.cargo/config.toml" <<'EOF'
[source.crates-io]
replace-with = "rsproxy-sparse"

[source.rsproxy-sparse]
registry = "sparse+https://rsproxy.cn/index/"
EOF
fi

cargo --version && rustc --version

export CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-2}"
export CARGO_PROFILE_RELEASE_LTO="${CARGO_PROFILE_RELEASE_LTO:-false}"
export CARGO_PROFILE_RELEASE_CODEGEN_UNITS="${CARGO_PROFILE_RELEASE_CODEGEN_UNITS:-8}"
if command -v mold >/dev/null 2>&1; then
  export RUSTFLAGS="${RUSTFLAGS:-} -C link-arg=-fuse-ld=mold"
fi

cargo build --release -p agent2api-providers
mkdir -p dist
cp -f target/release/agent2api-providers dist/
sha256sum dist/agent2api-providers
echo BUILD_OK
