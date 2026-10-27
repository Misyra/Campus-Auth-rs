# 校园网统一出口与 Clash / TUN 配置

## 在软件里选择出口

进入“方案 → 编辑方案 → 认证设置 → 校园网出口”。每个方案分别保存一份出口配置，浏览器 / HTTP 登录、重定向检测、自动与手动网络探测、认证入口预检和登录后验证都使用它。原来检测页和浏览器页的独立代理开关已收敛为此入口；更新下载、任务仓库和环境安装等公网业务不在此策略范围内。

| 模式 | 行为 |
|------|------|
| 直连（默认） | HTTP 请求忽略系统代理；Edge / Chrome / Chromium 使用禁用系统代理参数，Firefox 使用直连偏好。不修改系统 DNS，不保证绕过 TUN |
| 指定校园网网卡（IPv4） | 选择连接校园网的物理接口，登录和检测连接绑定该网卡，DNS 查询也从该接口发出；网卡不可用、绑定失败或 DNS 失败时明确报错，不回退其他接口或系统 DNS |
| 指定代理 | 支持 HTTP / SOCKS5，HTTP 可携带 Basic 认证。TCP 检测也经同一代理；`socks5h://` 将域名交给代理解析，适合保留原有 Clash SOCKS5 配置 |
| 系统默认 | 浏览器和 HTTP 请求允许使用系统代理，原生 TCP 使用系统路由。代理出口可达不代表校园网已完成认证 |

### 长期启用 TUN 的推荐设置

1. 选择“指定校园网网卡（IPv4）”，在下拉列表选择校园网的以太网或 Wi-Fi。列表显示当前 IPv4，刷新只更新清单，不会替换已选接口。
2. Windows 下会从所选网卡读取 DNS；如果网卡 DNS 已手动改成 `1.1.1.1`，请在“学校 DNS”里填写学校实际 DNS。Linux / macOS 当前需手动提供学校 DNS。每行一个 IPv4 地址，最多四个，也支持 `IPv4:端口`。
3. 保存方案。重定向检测也可以使用编辑器里尚未保存的出口草稿；手动登录与常规探测使用已保存方案。
4. 保持 TUN 开启验证登录和断线恢复。Windows 按接口 GUID 识别网卡，Unix 优先按 MAC 识别；执行前重新读取 IP、索引及 DNS，不把旧 DHCP 地址当作永久配置。

指定网卡通过受随机凭据保护的回环转发通道承载浏览器流量，HTTP / TCP 检测与 DNS 使用相同接口选择逻辑。该通道只监听 `127.0.0.1`，不会修改系统代理、DNS 或路由；浏览器出口更换、Worker 回收或租约释放后回收相应转发资源。HTTP 转发保留长连接与 Cookie / 令牌流程，HTTPS 使用 CONNECT 保持端到端 TLS。

TUN / VPN 的严格路由或防火墙仍可能阻断所选接口。遇到这种情况请配合下面的 Clash 直连规则，或临时暂停 TUN 对照测试；不要把“绑定源 IP”理解成能绕过所有 VPN 拦截。

脚本渠道的网络行为由脚本自身决定，程序无法强制任意子进程的每条连接绑定接口，所以保存时拒绝“脚本渠道 + 指定网卡”。需要强制统一出口时请使用浏览器或 HTTP 登录。浏览器对 SOCKS5 认证的支持取决于引擎，校园网场景优先使用无认证的本机 SOCKS5 或 HTTP 代理。

### 旧配置如何兼容

旧方案没有 `network` 字段时，在一个位置解析旧值：显式 `browser.bind_proxy` 优先；没有绑定代理且 `monitor.disable_proxy=false` 时选择系统默认；其他情况选择直连。方案编辑器回读有效策略，保存后形成显式的方案配置。新策略优先于旧字段，重定向检测也遵循同一策略；旧的 `redirect_test_use_proxy` 仅供旧 Worker / 无统一策略请求兼容，不再是主程序的独立设置入口。

导航或网络故障导致谨慎登录失败时，程序等待 300 秒冷却后允许重试，不必一直等待配置变化。密码错误、人工验证或选择器错误不会因此解除原有谨慎门禁；普通会话重试预算、连续失败冷却及暂停时段继续生效。

## Clash 配置作为补充方案

如果选择直连但仍开着 TUN，或 VPN 防火墙阻止指定网卡出口，可以按以下方法在 Clash / Mihomo 中设置绕过。不同客户端的覆写入口名称可能不同。

## 1. 保留 TUN，使用规则模式

Clash 全局模式直接使用 GLOBAL 策略，不匹配普通 `rules`。长期需要代理时，建议保留 TUN、切换到规则模式，把校园网与探测流量设为直连。希望其余流量默认走代理时，可将最后的 `MATCH` 指向已有代理组。

在客户端的覆写 / 合并配置中，将下面的直连项放到已有规则前面，避免订阅更新覆盖修改。示例不是完整订阅，不要用它覆盖节点、代理组或全部原有规则。`MATCH` 只保留一条并放在最后。

```yaml
mode: rule
rules:
  # 示例：替换为实际认证域名、认证服务器 IP 和学校 DNS
  - DOMAIN,auth.example.edu,DIRECT
  - IP-CIDR,10.0.0.1/32,DIRECT,no-resolve
  - IP-CIDR,10.0.0.53/32,DIRECT,no-resolve
  # 默认重定向触发地址与 HTTP / URL 检测目标
  - DOMAIN,www.msftconnecttest.com,DIRECT
  - DOMAIN,connect.rom.miui.com,DIRECT
  - DOMAIN,connectivitycheck.platform.hicloud.com,DIRECT
  - DOMAIN,wifi.vivo.com.cn,DIRECT
  - DOMAIN,captive.apple.com,DIRECT
  - DOMAIN,detectportal.firefox.com,DIRECT
  # 需替换为现有代理组的准确名称
  - MATCH,你的代理组
```

`auth.example.edu`、`10.0.0.1`、`10.0.0.53` 和“你的代理组”都是示例，必须替换。域名规则只填主机名，不填 `http://`、端口、路径或查询参数；IP 规则里的 `/32` 表示单个 IPv4 地址。门户跳转到其他认证域名时也要补充。

示例只涵盖默认 HTTP / URL 检测与重定向触发域名；自定义地址以当前方案和“设置 → 检测”为准。启用 TCP 检测时，还需为实际 TCP 目标添加直连规则。探测经代理时可能反映代理出口而非校园网的认证状态，因此只放行认证页还不够。

## 2. 校内域名使用学校 DNS

学校内部域名可能无法通过 `1.1.1.1` 解析。可以保留其他域名的公网 DNS，仅通过 Mihomo 的 `nameserver-policy` 将认证域名交给学校 DNS。学校 DNS 可向学校查询，或在关闭代理、网卡 DNS 自动获取时查看网络详情；不要把已手动设置的 `1.1.1.1` 当成学校 DNS。

将下面的字段合并进现有 `dns`，保留原有 DNS、策略及过滤项。以下 Fake-IP 示例适用于 `blacklist` 模式；若原配置使用 `rule` 或 `whitelist`，应按原模式语法让认证和触发域名返回真实 IP，不要直接切换模式。

```yaml
dns:
  nameserver-policy:
    'auth.example.edu': '10.0.0.53'
  fake-ip-filter-mode: blacklist
  fake-ip-filter:
    - 'auth.example.edu'
    - 'www.msftconnecttest.com'
```

如果未认证时公网 DNS 不可用，还需为实际触发和检测域名配置学校 DNS；学校 DNS 的请求本身也必须直连可达。浏览器自行启用的安全 DNS 可能绕过上述解析设置，排查时应一并检查。

部分门户依赖域名、HTTPS 证书或动态跳转参数，不要简单把 URL 中的域名替换成 IP。固定登录网址稳定时可在方案里填写；需要动态参数时仍应使用正常重定向流程。

## 3. 应用并验证

1. 应用覆写配置，确认客户端实际运行的是规则模式，且自定义规则没有被订阅覆盖。
2. 保持 TUN 开启，访问认证页，在 Clash 连接记录中确认认证、触发与检测目标使用 `DIRECT`。
3. 在认证喵运行重定向检测或手动登录，再检查校园网退出登录后是否能自动恢复；重定向测试需在未认证状态下进行。
4. 若仍打不开，可临时暂停 TUN、恢复学校 DNS，再测试以区分代理问题与登录任务问题；不能仅凭 TCP 端口可连接就认定认证成功。

必须保留全局模式时，Mihomo 可通过 `tun.route-exclude-address` 排除实际认证服务器等目标 IP，但 DNS、系统代理及检测目标仍需分别处理，域名对应 IP 变化后也要更新。仅添加普通 `DIRECT` 规则不能解决全局模式下的问题。

参考：[Mihomo 运行模式](https://wiki.metacubex.one/config/general/#运行模式)、[路由规则](https://wiki.metacubex.one/config/rules/)、[DNS 配置](https://wiki.metacubex.one/config/dns/)、[TUN 路由排除](https://wiki.metacubex.one/config/inbound/tun/#route-exclude-address)。
