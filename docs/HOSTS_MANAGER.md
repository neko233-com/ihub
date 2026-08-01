# hosts 文件管理

iHub 的 hosts 管理器只支持当前已验收的 Windows 10/11 x64 平台。读取固定路径 `%SystemRoot%\System32\drivers\etc\hosts` 不需要管理员权限；写入只在用户完成两步确认后发生。

## 管理模型

iHub 不把整份 hosts 当成自由文本覆盖。它只替换以下标记之间的映射：

```text
# >>> iHub managed hosts >>>
# Managed by iHub. Edit these entries in iHub so validation and backup remain active.
127.0.0.1 example.test # 本地开发
# ihub-disabled 0.0.0.0 paused.test
# <<< iHub managed hosts <<<
```

标记之外的注释、空行、映射和非 UTF-8 字节原样保留。外部启用映射已经使用的域名不能在 iHub 区重复，避免 hosts 的“前一行优先”造成看似保存、实际未生效的歧义。

## 保存流程

1. 工作台读取最多 1 MiB 的固定 hosts 文件，并计算 SHA-256 快照指纹。
2. 用户编辑最多 256 条映射；每条包含一个 IPv4/IPv6、1–8 个 ASCII 域名、可选备注和启用状态。hosts 不支持通配符，因此 iHub 也拒绝 `*`。
3. 第一次点击只显示写入预览；第二次确认才调用原生命令。
4. 原生层重新读取文件并比对指纹。其他程序在此期间改过文件时，写入立即中止，必须刷新后重新预览。
5. 当前进程没有写权限时，Windows 显示 UAC；用户批准后，同一个 iHub EXE 以 `--ihub-hosts-apply` 一次性辅助模式运行。辅助模式不会启动 Tauri、托盘、插件或后台服务。
6. 请求文件必须位于当前用户临时目录下固定的 `iHub-hosts-actions` 子目录，文件名必须等于请求 UUID；同时具有五分钟期限和 2 MiB 上限。父进程保持写句柄并只共享读取，UAC 等待期间不能被改写或替换。
7. 辅助模式再次校验固定 hosts 指纹，将临时文件写到同一个受保护目录并 `sync_all`，最后用 Windows `ReplaceFileW` 原子替换。上一份 hosts 保存为固定的 `hosts.ihub-backup`。

恢复备份采用相同的指纹、UAC 和原子替换流程。恢复后的旧当前文件会成为新的上一份备份，因此可以再切换一次；有未应用编辑时 UI 禁止恢复。

## 安全边界

- WebView 和插件都不能提交 hosts 路径、临时路径、原始文件字节或提权参数。
- IP、域名、备注、条目数、文件大小、请求大小和请求寿命都在 Rust 辅助模式中再次验证。
- UAC 取消、请求过期、备份缺失、指纹过期或原子替换失败都不会回退到直接截断原文件。
- 浏览器开发预览只显示静态示例，不读取系统文件，也不会触发 UAC。
- iHub 不刷新 DNS 缓存、不更改 DNS 服务器、不安装驱动，也不声称能够覆盖浏览器的 Secure DNS / DoH 行为。
