# Wi-Fi 密码查看器

iHub 的 Wi-Fi 密码查看器只支持当前已验收的 Windows 10/11 x64 平台。它直接使用 Windows Native Wi-Fi API，不启动 `netsh`、PowerShell 或外部 worker。

## 为什么每次需要 UAC

微软的 [`WlanGetProfile`](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlangetprofile) 文档规定：请求 `WLAN_PROFILE_GET_PLAINTEXT_KEY` 时，调用线程必须拥有 `wlan_secure_get_plaintext_key` 权限及 `WLAN_READ_ACCESS`；本机默认只向 Administrators 组授予明文密钥权限。iHub 因此不会把“未提权时返回的加密 `keyMaterial`”冒充密码，也不会修改系统 DACL。

## 读取流程

1. 工作台通过 `WlanOpenHandle`、`WlanEnumInterfaces` 和 [`WlanGetProfileList`](https://learn.microsoft.com/en-us/windows/win32/api/wlanapi/nf-wlanapi-wlangetprofilelist) 枚举最多 32 个适配器、合计 512 个已保存配置。
2. 列表读取不带明文标记的 profile XML，只提取认证、加密方式和是否存在 `sharedKey`。列表响应没有 `keyMaterial`。
3. 用户选择一个具有预共享密钥的配置并点击“请求查看”。普通权限下 Windows 显示 UAC，取消即停止。
4. 同一个 iHub EXE 进入一次性 `--ihub-wifi-reveal` 辅助模式，只接受 128-bit 不透明配置 ID 和格式固定的本机管道名，不启动 Tauri、托盘、插件或常驻服务。
5. 父进程先创建 `\\.\pipe\ihub-wifi-<随机 UUID>` 的只入站、拒绝远程、单实例命名管道；辅助程序连接后，父进程用 `GetNamedPipeClientProcessId` 核对连接者就是刚刚由 UAC 启动的进程。连接、读取和辅助进程各有 30 秒边界。
6. 辅助程序只为所选配置调用带 `WLAN_PROFILE_GET_PLAINTEXT_KEY` 的 `WlanGetProfile`。只有 `<sharedKey><protected>false</protected><keyMaterial>…` 才会返回；企业 802.1X/EAP 凭据、无密码网络和仍受保护的 XML 都会拒绝。
7. XML 实体经过有界解析；Native Wi-Fi 返回的 UTF-16 缓冲、Rust XML、JSON 与管道字节在使用后主动清零。响应不写临时文件、不进入 host log，也不向插件开放。

## 界面生命周期

- 密码默认显示，用户可隐藏；切换配置、关闭工作台、点击“立即清除”或 60 秒倒计时结束都会从 React 状态移除。
- 只有点击“复制密码”才写入系统剪贴板。iHub 不自动清空剪贴板，因为那可能覆盖用户后来复制的新内容；复制后的清理由用户负责。
- 浏览器开发预览只显示静态配置元数据，不枚举真实 SSID、不触发 UAC，也绝不提供示例密码。
- iHub 不连接、断开、删除、导入或修改 WLAN 配置，不扫描附近网络，也不尝试绕过企业策略。
