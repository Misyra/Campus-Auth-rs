# OpenWrt 部署指南

OpenWrt 版是独立的无桌面构建，保留 Web 控制台、HTTP 直连登录、自定义脚本、网络监测与定时任务。它不链接 GTK / AppIndicator，也不提供浏览器登录、录制器、浏览器任务调试、OCR 或自动安装 Python 环境。Windows、macOS 与普通 Linux 桌面版保持原有功能。

## LuCI 插件安装

插件拆成两个软件包：`campus-auth` 安装主程序、procd 服务及 UCI 配置，`luci-app-campus-auth` 安装“服务 → 认证喵”页面。LuCI 页面提供运行状态、启动 / 停止 / 重启、开机启动开关、监听地址 / 端口 / 数据目录及控制台入口。校园网账号、HTTP / 脚本任务与自动监测仍在内置认证控制台配置，LuCI 不保存校园网密码。

OpenWrt 24.10 及更早版本使用 opkg / IPK；25.12 起改用 APK，不能把 `.ipk` 改名成 `.apk`。见 [OpenWrt 软件包文档](https://openwrt.org/packages/start)。目前提供 IPK 直接打包脚本及两种格式共用的 SDK 包定义；APK 必须用对应固件 SDK 生成。源码包含这些打包入口，实际下载包须等待交叉构建成功，不能将源码目录当作可安装插件。

对于 opkg 固件，先通过 `opkg print-architecture` 核对架构。将匹配设备的两个 IPK 和摘要传至 `/tmp`，在路由器 SSH 终端执行（示例为 x86_64）：

```sh
cd /tmp
sha256sum -c campus-auth_5.1.2-1_x86_64.ipk.sha256
sha256sum -c luci-app-campus-auth_5.1.2-1_all.ipk.sha256
opkg update
opkg install ./campus-auth_5.1.2-1_x86_64.ipk ./luci-app-campus-auth_5.1.2-1_all.ipk
```

安装后刷新或重新登录 LuCI，在“服务 → 认证喵”查看状态，按需点击“开启开机启动”。IPK 首次安装启动服务，但不强制开启开机启动；升级保留已有开机启动开关。SDK 包使用固件标准的服务安装钩子，安装后检查实际开机启动状态。包依赖 `ca-bundle`、`ip-full`、`luci-base` 和 `rpcd-mod-file`，离线设备需一并准备对应固件的依赖包。

SDK 生成的 APK 在 APK 固件上使用 `apk add /tmp/实际主程序包.apk /tmp/实际LuCI包.apk` 安装。自行构建且未签名的包，需要核对来源和摘要后加 `--allow-untrusted`；正式分发应使用自己的签名和仓库。包名、版本及架构以实际产物为准。IPK / APK 均可通过固件的软件包页面上传安装。

默认回环监听时，页面提示使用 SSH 隧道；如需“打开认证控制台”按钮，在插件中把监听地址设为路由器 LAN IP，保存并应用后点击重启服务，刷新页面。控制台是独立 HTTP 服务，不继承 LuCI 的登录会话，也不自动创建防火墙放行规则。

## 设备与架构

| 设备 CPU | 构建目标 | 说明 |
|---|---|---|
| x86_64 软路由 | `x86_64-unknown-linux-musl` | 64 位小端 |
| ARM64 路由器 | `aarch64-unknown-linux-musl` | 64 位小端 ARMv8 |
| ARMv7 路由器 | `armv7-unknown-linux-musleabihf` | 32 位小端，硬浮点 ABI；不能用于 ARMv5/ARMv6 或软浮点设备 |
| MIPS / MIPSel / 其它架构 | 暂无标准包 | 必须结合具体 CPU、浮点 ABI、原子指令能力和 OpenWrt SDK 单独验证；不能改包名冒充兼容 |

SSH 登录路由器后执行 `uname -m` 与 `ubus call system board` 确认设备。CI 已配置前三种目标的交叉编译、ELF 无动态加载器及动态库检查，以及原生 / QEMU `--version` 执行检查。这些检查需在推送后实际通过，且不能代替硬件上的校园网登录实测；还需要核对具体固件、可用内存、闪存空间与门户任务。

只需要 HTTP 或 shell 脚本登录时，无需安装 Python、Chromium、uv 或桌面库。JavaScript 凭据变换仍由内置引擎完成，不需要 Node.js。脚本任务使用 `/bin/sh`、系统 Python 或其它已安装的解释器；Python 依赖由用户在设备上管理。

## 获取与构建

插件安装使用设备架构对应的 `campus-auth_*.ipk` 与架构无关的 `luci-app-campus-auth_*_all.ipk`，或者对应 SDK 生成的 APK。手动部署也可选择文件名包含 `openwrt` 且架构匹配的 `.tar.gz`，并核对旁边的 `.sha256` 文件。普通 `*-linux-gnu` 发布包依赖 glibc 和桌面库，不能作为 OpenWrt 包使用。

在装有项目指定 Rust 工具链、Node.js、npm、Python、binutils 的 Linux 开发机上构建。安装交叉构建工具并添加所需 Rust 目标：

```sh
python3 -m pip install 'cargo-zigbuild==0.23.4' 'ziglang==0.14.1'
rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl armv7-unknown-linux-musleabihf
npm --prefix frontend ci
bash openwrt/build.sh aarch64-unknown-linux-musl
```

将最后一个参数替换为设备对应目标。产物位于 `openwrt/dist/`，包括静态二进制的便携包、两个 IPK 与 SHA256。脚本将 `VITE_OPENWRT=true` 的前端构建到独立的 `frontend/dist-openwrt/`，Rust 使用 `--no-default-features --features openwrt` 嵌入这一目录，不覆盖桌面版 `frontend/dist/`。

默认 IPK 架构名分别为 `x86_64`、`aarch64_generic`、`arm_cortex-a7_neon-vfpv4`，只是常用包架构示例。ARM64 / ARMv7 的 OpenWrt 架构名还包含 CPU 子型号；例如 ARM64 设备可能使用 `aarch64_cortex-a53`。以 `opkg print-architecture` 为准，通过环境变量指定设备架构：

```sh
OPENWRT_PKG_ARCH=aarch64_cortex-a53 bash openwrt/build.sh aarch64-unknown-linux-musl
```

若静态二进制已经构建，可单独打包，避免重复构建前端和 Rust：

```sh
python3 openwrt/package_ipk.py --binary target/aarch64-unknown-linux-musl/release/campus-auth \
  --target aarch64-unknown-linux-musl --architecture aarch64_cortex-a53
```

打包前验证 ELF 架构、ARM 硬浮点标记及动态加载器 / 动态库依赖。架构名称匹配不替代具体 CPU 和固件的运行验证；不能使用 `--force-depends` 或修改架构为 `all` 绕过设备不兼容。

仅检查无桌面代码时可执行 `cargo check --locked --no-default-features --features openwrt,no-embed`。正式包不能启用 `no-embed`。不应使用 `--all-features` 构建路由器包，因为这会重新启用桌面依赖。

交叉构建工具说明：[cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild)。

## 使用固件 SDK 生成原生包

先下载与设备固件版本、target / subtarget 相同的 OpenWrt SDK，在 Linux 开发机安装 SDK 要求的依赖及 Python 3.11 或更高版本。两份 `Makefile` 使用标准 `package.mk`，由 SDK 自动选择 IPK 或 APK，并生成设备的包架构；主程序使用前一步构建的静态二进制，不调用 SDK 自带的 Rust 工具链。

在本仓库执行以下命令，输出目录必须尚不存在：

```sh
python3 openwrt/prepare_sdk.py openwrt/dist/sdk-package
```

组装器将 `openwrt/package/` 与公共服务 / 配置 / 备份模板合并。将生成的两个包目录复制到 SDK 的 `package/`，再将目标架构的 `campus-auth` 静态二进制复制为 SDK 内 `package/campus-auth/campus-auth`。不要直接复制未组装的 `openwrt/package/campus-auth`，否则缺少公共部署文件。

在 SDK 根目录执行：

```sh
./scripts/feeds update -a
./scripts/feeds install luci-base rpcd-mod-file ip-full ca-bundle
make menuconfig
# 在 Network 选择 campus-auth，在 LuCI → Applications 选择 luci-app-campus-auth 为 M。
make package/campus-auth/clean
make package/campus-auth/compile V=s
make package/luci-app-campus-auth/compile V=s
find bin/packages -name '*campus-auth*.ipk' -o -name '*campus-auth*.apk'
```

SDK 编译步骤再用目标 `readelf` 校验二进制。使用与固件相同的 SDK 还可避免依赖包 ABI 不一致。MIPS、ARMv5 / ARMv6、软浮点或其它未支持目标会被当前包检查拒绝，需要增加对应构建目标并在设备上验证后才可分发。

CI 另外配置了固定 24.10.0 / 25.12.0 SDK 的 x86_64 打包检查，分别生成 SDK IPK / APK，下载前核对官方 SDK SHA256；这些版本用于验证包格式。其它设备仍应使用自己固件对应的 SDK。SDK 检查须等待 CI 实际通过，当前不代表真机安装与校园网链路已验证。

## 安装与服务管理

将包传到路由器并解压到独立的 `/opt/campus-auth`；空间不足时可选择持久化的外部存储路径，同时修改 UCI 的 `binary`。以下安装操作在路由器的 SSH 终端执行：

```sh
mkdir -p /opt/campus-auth
tar -xzf /tmp/campus-auth-v5.1.2-openwrt-aarch64-unknown-linux-musl.tar.gz -C /opt/campus-auth
chmod 755 /opt/campus-auth/campus-auth
cp /opt/campus-auth/openwrt/files/campus-auth.init /etc/init.d/campus-auth
chmod 755 /etc/init.d/campus-auth
cp /opt/campus-auth/openwrt/files/campus-auth.config /etc/config/campus-auth
uci set campus-auth.main.binary='/opt/campus-auth/campus-auth'
uci commit campus-auth
/etc/init.d/campus-auth enable
/etc/init.d/campus-auth start
```

上述步骤仅适用于未使用包管理器的手动部署。原生软件包将主程序安装至 `/usr/bin/campus-auth`，无需手动复制服务 / 配置。包名中的版本和架构以实际下载为准。首次手动安装才复制 UCI 示例；升级时保留 `/etc/config/campus-auth`。不要在已经安装原生包的设备上混用手动部署。

程序由 procd 以前台模式管理，开机启动、异常退出重试及停止信号均交给 procd。数据位于 `/etc/campus-auth/`，首次创建的配置默认关闭文件日志和联网更新检查，日志进入 `logread`，降低闪存写入。网络自动监测需要在 Web 控制台完成账号及任务配置，并开启监测 / 设置启动动作。

数据目录必须是专用目录；服务会解析符号链接并拒绝 `/etc`、`/root` 等系统目录，避免收紧数据目录权限时影响系统。自定义外部存储可使用 `/mnt/持久挂载点/campus-auth`，并在更改前迁移旧数据。

```sh
/etc/init.d/campus-auth restart
/etc/init.d/campus-auth stop
/etc/init.d/campus-auth disable
logread -e campus-auth
ubus call service list '{"name":"campus-auth"}'
```

服务启动方式遵循 [OpenWrt procd 文档](https://openwrt.org/docs/guide-developer/procd-init-scripts)。服务脚本允许 40 秒优雅关闭，程序已有 SIGTERM 处理。控制台重启与定时重启通过退出进程让 procd 接管，避免自行启动第二个实例；要持续停机应使用 `/etc/init.d/campus-auth stop`，控制台关闭进程后 procd 仍会重启。

## 打开控制台

默认监听路由器的 `127.0.0.1:50721`。在电脑上建立 SSH 隧道：

```sh
ssh -L 50721:127.0.0.1:50721 root@192.168.1.1
```

然后访问电脑上的 `http://127.0.0.1:50721`，完成首次向导，导入或创建 HTTP 登录任务，再将账号方案绑定到任务。已有浏览器方案不会自动转换为 HTTP 请求，应明确选择 HTTP 或脚本并绑定对应任务。

需要直接从 LAN 访问时，可将监听地址改为路由器实际 LAN IP：

```sh
uci set campus-auth.main.host='192.168.1.1'
uci commit campus-auth
/etc/init.d/campus-auth restart
```

随后访问 `http://192.168.1.1:50721`。仅在可信 LAN 放行这个端口，不配置 WAN 入站或公网端口转发；控制台可修改凭据和执行脚本。CLI 的 host / port 由 UCI 覆盖，修改它们应编辑 UCI 后重启服务。

## 登录与网络环境

- HTTP 任务见 [直连登录指南](http-login-guide.md)；校园网必须支持从路由器 WAN 出口认证，通常 LAN 客户端通过 NAT 共用已认证出口。
- 脚本登录见 [自定义脚本指南](custom-script-guide.md)。必须使用设备上可执行的解释器；不要选择默认的自动管理 Python 环境。退出码与 `CAMPUS_*` 凭据环境变量保持原有契约。
- 网络枚举使用设备上的 `ip addr show` 和 `ip route show default`，优先取默认路由出口的 IP / MAC，避免误用 LAN。缺少兼容的 `ip` 命令时安装固件对应的 iproute2 包（常见为 `ip-full`）；WiFi 中继场景需进一步验证固件的无线工具与 SSID 获取能力。
- 涉及验证码或只能通过浏览器操作的门户，在此版本中不能自动认证，应先评估能否改写为 HTTP / 脚本任务。
- 校园网绑定设备 MAC、禁止共享或使用 802.1X 等情况，需要按学校网络实际配置处理；本工具的 HTTP 登录不能代替 802.1X 客户端。

## 备份、升级与卸载

备份 `/etc/config/campus-auth`、整个 `/etc/campus-auth` 和运行用户的 `~/.campus_network_auth`。默认 root 用户的密钥目录是 `/root/.campus_network_auth`；只有配置文件没有密钥，备份中的加密密码无法恢复。原生包通过 conffiles 保留 UCI，`/lib/upgrade/keep.d/campus-auth` 声明默认数据与密钥的 sysupgrade 备份。自定义数据目录、非 root 密钥目录或手动部署须将实际路径加入 `/etc/sysupgrade.conf`；程序安装目录或外部存储按自己的固件升级策略处理。

OpenWrt 版保留版本检查，但禁用桌面更新助手的在线安装、手动上传安装及待更新自动应用，也禁用 Web 卸载。原生包升级使用 `opkg install 新的两个IPK` 或固件的 APK 包管理器；手动部署升级前先停止服务，替换 `/opt/campus-auth/campus-auth` 与包附带的部署文件，保留 UCI、数据和密钥，然后启动服务。升级过程中不要覆盖已有配置。

原生插件卸载使用 `opkg remove luci-app-campus-auth campus-auth` 或 `apk del luci-app-campus-auth campus-auth`；卸载服务会停止进程，数据与密钥目录不在安装文件清单内，默认保留。UCI 文件由包管理器按配置文件规则处理，重要配置仍应预先备份。手动部署卸载时先执行 `stop` 和 `disable`，再删除专用程序目录、`/etc/init.d/campus-auth` 与 `/etc/config/campus-auth`。需要保留账号任务时，同时保留数据与密钥目录；只有确认不再需要凭据才删除它们。
