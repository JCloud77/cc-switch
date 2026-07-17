# 坚果云 WebDAV 静态解析安全测试

> 仅用于已授权的终端安全控制验证。该功能默认关闭，并会在启用时写入审计日志。

## 用途

当安全团队需要验证 DNS/FQDN 关联型终端策略时，可以让 CC Switch 的 HTTP 客户端仅对
`dav.jianguoyun.com` 使用指定的测试 IP。该覆盖：

- 不修改请求 URL；
- 不关闭 TLS 证书或 SNI 校验；
- 不影响其他域名；
- 不会隐藏启用状态；
- 配置 CC Switch 显式全局代理时不会生效。

## 启用

完全退出 CC Switch，然后从同一个 PowerShell 会话设置：

```powershell
$env:CC_SWITCH_SECURITY_TEST_MODE = "1"
$env:CC_SWITCH_SECURITY_TEST_JIANGUOYUN_IPS = "<经安全团队批准的IP1>,<经安全团队批准的IP2>"
& "C:\path\to\cc-switch.exe"
```

IP 列表支持英文逗号或分号分隔，只接受 IP 地址。拒绝回环、未指定地址和组播地址。

启动后检查：

```text
%USERPROFILE%\.cc-switch\logs\cc-switch.log
```

预期出现：

```text
[SecurityTest] AUDIT: static DNS override active for dav.jianguoyun.com: ...
```

随后在 CC Switch 的 WebDAV 设置中执行“测试连接”。使用公司控制的测试账号和无敏感
canary 数据验证 `PROPFIND`、`MKCOL`、`PUT`、`HEAD`、`GET` 的检测与阻断结果。

## 关闭

退出 CC Switch，并在 PowerShell 中执行：

```powershell
Remove-Item Env:CC_SWITCH_SECURITY_TEST_MODE -ErrorAction SilentlyContinue
Remove-Item Env:CC_SWITCH_SECURITY_TEST_JIANGUOYUN_IPS -ErrorAction SilentlyContinue
```

正常方式重新启动 CC Switch。未设置环境变量时，程序行为与原版本一致。

## 限制

- CDN IP 可能变化，不应将测试值长期部署为生产配置。
- 如果设置了 CC Switch 自身的全局代理，解析通常发生在代理端，本地静态解析会被忽略。
- 该测试只能证明某条终端控制链路是否可被不同解析路径影响，不能代替 SWG、DLP、EDR
  和出口日志的联合验证。
