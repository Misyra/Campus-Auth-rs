#!/usr/bin/env bash
# 在开发机或 CI 上交叉编译并组装静态 musl 包，路由器上无需 Rust/Node/Python。
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_dir"
target="${1:-x86_64-unknown-linux-musl}"
case "$target" in
    x86_64-unknown-linux-musl|aarch64-unknown-linux-musl|armv7-unknown-linux-musleabihf) ;;
    *) echo "不支持的目标: $target；MIPS 等设备须使用对应 OpenWrt SDK 单独验证" >&2; exit 1 ;;
esac
command -v readelf >/dev/null || { echo "缺少 readelf，请安装 binutils" >&2; exit 1; }
cargo zigbuild --help >/dev/null
(
    cd frontend
    VITE_OPENWRT=true npm run build -- --outDir dist-openwrt
)
# ARM musl 也显式开启静态 CRT，禁止意外依赖设备上的动态加载器。
RUSTFLAGS="${RUSTFLAGS:-} -C target-feature=+crt-static" \
    cargo zigbuild --release --locked --target "$target" --no-default-features --features openwrt --bin campus-auth

binary="${CARGO_TARGET_DIR:-target}/$target/release/campus-auth"
python3 openwrt/verify_elf.py "$binary" --target "$target"
version="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -n 1)"
[ -n "$version" ] || { echo "无法读取主程序版本" >&2; exit 1; }
out="$repo_dir/openwrt/dist"
mkdir -p "$out"
stage="$(mktemp -d "$out/.package.XXXXXX")"
trap 'rm -rf -- "$stage"' EXIT
cp "$binary" "$stage/campus-auth"
chmod 755 "$stage/campus-auth"
mkdir -p "$stage/openwrt/files"
cp openwrt/files/* "$stage/openwrt/files/"
cp docs/guides/openwrt-guide.md "$stage/README.md"
cp docs/guides/http-login-guide.md docs/guides/custom-script-guide.md "$stage/"
cp LICENSE "$stage/LICENSE"
archive="campus-auth-v${version}-openwrt-${target}.tar.gz"
tar -czf "$out/$archive" -C "$stage" .
(
    cd "$out"
    sha256sum "$archive" > "$archive.sha256"
)
echo "OpenWrt 安装包: $out/$archive"
# IPK 架构名须匹配固件；ARM 的默认包只是常见设备示例，不代表全架构通用。
ipk_args=()
[ -z "${OPENWRT_PKG_ARCH:-}" ] || ipk_args+=(--architecture "$OPENWRT_PKG_ARCH")
python3 openwrt/package_ipk.py --binary "$binary" --target "$target" --output "$out" "${ipk_args[@]}"
