# 开发机运行与更新

本页把“开发机一直使用最新版本”拆成四个有意不同的动作：**运行当前工作树**、**尽可能安全地跟随上游**、**显式同步上游源码**与**显式把当前工作树安装到本机**。这样既能让正在编辑的代码立即生效，也不会因为启动应用而丢失本地修改。

文中的 “Always Latest, Safe” 是安全更新模式的名称，不是无条件覆盖承诺：工作树脏、领先或分叉、没有 upstream、网络／构建失败，或精确的已安装 iHub 仍在运行时，脚本都会保留当前源码或已安装版本并报告／等待，不会为了追求版本号而破坏这些边界。

## 当前源码启动（推荐）

前置条件是 Node.js 22.12+（含 Corepack）、Rust stable、Git，以及对应平台的 Tauri 系统前置依赖。脚本使用 `package.json` 中锁定的 pnpm 版本；依赖安装始终带 `--frozen-lockfile`，不会升级或改写 lockfile。

Windows：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\dev.ps1
```

macOS：

```bash
bash ./scripts/dev.sh
```

两个脚本都会从**当前检出的工作树**启动 `tauri dev`，随后由 Vite/Tauri 监视源码并重新加载。因此保存本地改动后，下一次重载就是开发机所使用的最新源码；它不是一个会落后于工作树的已安装副本。需要让开发机在每次启动时尽可能追随远端时，使用安全模式：

插件中心还会从开发安装器写入的可信 `sourceRoot` 验证 `plugins/official/` 下固定 ID 的 18 个第一方独立 checkout。未安装项目会优先显示“链接源码”，已有受管 Git 快照可显式“切到源码”；离开可信开发安装后，全部条目都回退到锁定 commit 的 Git 包。开发脚本会从 registry lock 补齐缺失 checkout；只有显式更新模式才会尝试安全 fast-forward，脏、领先或分叉的插件仓一律保留。源码关联只读取各插件当前的已构建 `dist/` / `bin/`，不会替开发者运行任意仓库脚本。

```powershell
.\scripts\dev.ps1 -UpdateIfClean
```

```bash
bash ./scripts/dev.sh --update-if-clean
```

它只在工作树干净、已有 upstream、可严格 fast-forward 时执行 `fetch` 和 fast-forward；遇到未提交改动、分叉、领先上游、没有 upstream 或网络问题时，会保留当前源码并继续启动，不会 reset、checkout、clean 或覆盖工作区。若希望这些情况改为明确失败，使用严格的 `-Update` / `--update`。

默认流程为：检查 Node/Rust/Git → 根据 `pnpm-lock.yaml` 同步依赖 → 运行 TypeScript 检查 → 启动桌面端。仅做无窗口环境检查可使用：

```powershell
.\scripts\dev.ps1 -VerifyOnly
```

```bash
bash ./scripts/dev.sh --verify-only
```

`-Build` / `--build` 构建当前源码但不生成安装包；`-Package` / `--package` 生成本机安装包，并把产物放在 `src-tauri/target/release/bundle/`。Windows 需要把**当前工作树**安装到本机时，使用 `-InstallLatest`：它会先仅清除当前配置精确推导出的 NSIS installer、`.sig` 与证明文件（拒绝目录和 reparse point），再打包；只有同一次构建重新生成与当前 `tauri.conf.json` 完全对应的 `nsis/<product>_<version>_x64-setup.exe` 及其 Tauri updater `.sig`，才会校验固定的 `%LOCALAPPDATA%\<product>\<mainBinary>.exe` 目标后以 `/S` 安装。

Tauri 为 NSIS 打包时会把主程序的 bundle marker 临时改为 `NSS`，makensis 结束后再把 `target/release/<mainBinary>.exe` 恢复为未打包标记；所以最终 release 文件与安装载荷的 SHA-256 按设计不同。iHub 的 installer hook 会在 makensis 读取载荷时，把这个 **NSS-patched 且保持 `<mainBinary>.exe` 文件名**的输入复制到同一受控构建目录的不可变快照，并生成包含 SHA-256、长度、随机 nonce 和时间的证明。安装器把相同三元组写入新的安装后 marker；`-InstallLatest` 在启动前复核快照与安装器，在返回后复核 marker 的新鲜度、nonce、长度，以及精确安装目标的 SHA-256，全部一致才报告成功。同名要求不可删除：Tauri 的 NSIS 模板不使用 `/oname`，改名快照会被安装成旁路文件而不能替换主程序。流程不拉取、重置、检出或清理 Git；若精确已安装路径正在运行，则会停止流程并要求你自行关闭，绝不会按进程名批量结束应用。

```powershell
# 关闭已安装的 iHub 后执行；这会安装当前已保存的本地源码，不会同步 Git。
.\scripts\dev.ps1 -InstallLatest
```

macOS 的等价显式开发安装命令是：

```bash
# 会安装到当前用户的 ~/Applications/iHub.app，不使用 sudo，也不会启动应用。
bash ./scripts/dev.sh --install-latest
```

它要求同一次构建产生 `macos/<product>.app`、`<product>.app.tar.gz` 与其 Tauri updater `.sig`，然后通过同目录的 staging/previous 原子替换只更新 `~/Applications/iHub.app`。它拒绝符号链接路径；发现名为 `ihub` 的进程时会保守地要求用户自行关闭，绝不结束进程。

如果目标是让**已安装的开发副本**随本地保存的源码持续保持最新，需要显式启动 watch/install，而不是依赖默认启动器：

```powershell
# Windows；Ctrl+C 停止。可用 -WatchIntervalSeconds 5 调整轮询。
.\scripts\dev.ps1 -WatchInstall
```

```bash
# macOS；Ctrl+C 停止。可用 --watch-interval-seconds 5 调整轮询。
bash ./scripts/dev.sh --watch-install
```

两种 watch 模式只监测当前工作树中 Git 已跟踪或未被忽略的**已保存路径**，用路径、文件长度与最后修改时间的元数据做快速快照；它是防抖触发器，不替代安装包签名验证。独立嵌套插件 Git 仓库只作为目录边界，不会在根项目 watcher 中递归扫描。每次变更经过一次稳定轮询后，才会复用各自的安全打包/安装流程。Windows 会按精确安装目标路径获取有界的全局命名互斥，`-Package`、`-InstallLatest` 与多个 watcher 因而不能同时清理／构建／安装同一个 NSIS 产物；锁超时只会让 watcher 在下次轮询重试。它们永不 fetch、pull、reset、checkout、clean、启动 iHub 或结束任何进程。运行中的 iHub 只会使 watcher 等待；构建失败会保留当前已安装版本，并按 30 秒、2 分钟、5 分钟做三次有界自动重试；持续失败后才等待下一次保存的源码变更，避免无限编译循环。持久 Windows watcher 还会把失败命令最近的有界输出写入状态文件，便于诊断无可见控制台的后台构建。watcher 不会把本机开发产物伪装成 GitHub Release 更新。

本机流程会拒绝缺失本次构建 `.sig` 的包，绝不会静默降级为未签名包；它验证的是本次构建是否产出成对的 updater artifact，不是对 GitHub Release、`latest.json` 或已安装旧版本更新链路的端到端验收。Tauri 的 updater 私钥密码是**可选**的：没有密码的私钥也能签名，只有加密私钥才需要密码。

Windows 的 `-Package` 优先使用 CI 已注入的 `TAURI_SIGNING_PRIVATE_KEY`（可为私钥内容或文件路径）；若私钥已加密，再额外提供 `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`。本机未设置时，它会读取当前用户的 `%LOCALAPPDATA%\iHub\keys\tauri-updater-release-v2.key`；同目录 `.password` 文件存在时才读取密码。可通过 `IHUB_UPDATER_PRIVATE_KEY_PATH`、`IHUB_UPDATER_PASSWORD_PATH` 覆盖。macOS 不创建本地密钥，要求显式提供 `TAURI_SIGNING_PRIVATE_KEY`，或至少提供 `IHUB_UPDATER_PRIVATE_KEY_PATH`；密码文件同样可选。私钥和密码不能进入 Git、Release 资产或日志；正式发布时将私钥内容放入 GitHub Actions 的 `TAURI_SIGNING_PRIVATE_KEY` secret，若私钥加密再配置密码 secret。

`-SkipInstall`、`-SkipCheck`（macOS 为对应的 `--skip-*`）只适合已明确知道环境完好的离线或调试场景。

## 显式、安全地同步上游

启动脚本的默认无参数模式**绝不会**执行 `git fetch`、`git pull`、`reset`、`checkout` 或 `clean`。需要把无法安全更新视为错误时，由开发者明确请求一次严格同步：

```powershell
.\scripts\dev.ps1 -Update
```

```bash
bash ./scripts/dev.sh --update
```

该选项先要求工作树完全干净且当前分支已配置 upstream，然后才 `git fetch --prune`，并且只接受严格 fast-forward。若存在未提交文件、没有 upstream、分支领先上游、分叉或需要合并，严格模式会停止，不会覆盖、回滚或合并你的代码。先自行审查 `git status` / `git log`，处理完本地工作后再重试；日常“尽量最新”可使用上面的 `UpdateIfClean` 模式。

## 本机安装：稳定版与开发版

稳定版使用经校验的 GitHub Release 安装器，Windows 运行 `scripts/install.ps1`，macOS 运行 `scripts/install.sh`；具体命令、SHA-256 校验和签名策略见[发布与安装](RELEASE.md)。两个安装器都强制要求同一 Release 中存在 `SHA256SUMS.txt`，且其中包含所选安装包的有效条目；清单缺失、条目不匹配或校验失败时，它们都会停止而不是安装未经验证的内容。

Windows 开发机即使没有 GitHub Release，也可以创建一个**当前源码启动器**和用户级开始菜单入口：

```powershell
Set-ExecutionPolicy -Scope Process Bypass
.\scripts\install-dev.ps1 -NoLaunch
```

它会写入 `%LOCALAPPDATA%\iHub Development\`，并创建五个开始菜单快捷方式：

- **iHub Development (Always Latest, Safe)**：每次启动先在安全条件下跟随 upstream；工作树有改动、分叉、领先上游、没有 upstream 或网络不可用时，保留当前保存的源码并继续启动。
- **iHub Development (Current Source)**：不复制源码、不覆盖稳定版、也不更新 Git；每次点击都调用所配置工作树中的 `scripts/dev.ps1`，因此运行的一定是这份工作树当前保存的代码。
- **iHub Development (Update & Launch)**：先请求一次安全 fast-forward，再启动。工作树有改动、没有 upstream 或已经分叉时会安全停止，绝不覆盖本地代码。
- **iHub Development (Install Current Build)**：从该工作树构建本次生成 Tauri updater sidecar 的本地 NSIS 包，并且只在包名、重新生成的 `.sig`、用户安装目录与主可执行文件都符合当前配置时才静默安装。它不会更新 Git、覆盖任意路径或停止进程；已安装 iHub 正在运行时需先由你自行关闭。安装完成后可从通常的 **iHub** 开始菜单项打开安装版。
- **iHub Development (Watch & Install Current Build)**：仅在用户主动点击后持续监测本地保存的源码，并在稳定变更后复用同一套精确 NSIS 和进程安全检查来刷新安装版。它从不更新 Git、启动 iHub 或结束进程；按 `Ctrl+C` 停止。

安装脚本只会复用带有 iHub 自身 marker 的用户目录；遇到同名的其他目录时会拒绝覆盖。移动工作树后，从新位置重新运行此命令即可更新启动器指向。

默认安装后会通过 `UpdateIfClean` 启动；`-NoLaunch` 仅创建入口。`-Update` 是显式传递给开发启动器的严格 fast-forward 请求；若与 `-NoLaunch` 组合，则会安全更新并做依赖/TypeScript 验证，但不打开窗口。`-UpdateIfClean` 会在无法安全更新时继续验证／启动当前工作树。`-InstallLatest` 与 `-WatchInstall` 都是完全本地的显式流程，不能与两种更新模式组合；即使启动器以 `-NoLaunch` 配合其中之一运行，也只会完成你明确请求的打包／安装，不会启动 iHub。macOS 开发时可使用 `bash ./scripts/dev.sh --update-if-clean` 来保持同样的安全跟随上游语义，或使用上面的 `--install-latest` / `--watch-install` 安装当前用户副本。如果需要把当前工作树交给本机其他账户或以接近发布的方式验证，先运行 `-Package` / `--package`，再从产物目录手动执行对应平台安装包。这样安装版和开发版的用途明确分离：开发版不替换稳定版，稳定版也不会伪装成热更新的工作树。

### 可选的 Windows 持久开发安装服务

默认**不会**创建后台服务或计划任务。需要让这一个受信任工作树在登录后持续“安全跟随上游 + 本地变更后安装”时，先创建／刷新开发启动器，再明确启用：

```powershell
.\scripts\install-dev.ps1 -NoLaunch
.\scripts\install-dev.ps1 -EnablePersistentDevelopmentInstall -UpstreamCheckMinutes 30
```

启用前会校验 `%LOCALAPPDATA%\iHub Development\launcher.json` 的 iHub marker，并确认它指向当前工作树；当前的 verified-install 状态协议要求 marker 的 `launcherRevision` 至少为 `3`。旧版 marker 会在 `-DevelopmentInstallStatus` 中显示为 `refresh-required`，且不能用于启用任务；升级后先重新运行 `.\scripts\install-dev.ps1 -NoLaunch` 即可安全刷新。旧工作树、丢失启动器或同名的非 iHub 任务都会拒绝覆盖。可先无副作用地预览：

```powershell
.\scripts\install-dev.ps1 -EnablePersistentDevelopmentInstall -WhatIf
```

它只为**当前 Windows 用户**注册两个非提权的登录触发任务，使用 `Interactive` / `Limited` 令牌、`PowerShell -File` 的精确本地 wrapper 路径和 `IgnoreNew` 单实例策略：

- **iHub Development - Watch & Install**：监测这份工作树的已保存源码；只有源码稳定、包与 updater sidecar 重新生成并通过既有本地校验，且你已经自行关闭精确的已安装 iHub 后，才会替换安装版。
- **iHub Development - Safe Upstream Refresh**：在登录后循环尝试 `UpdateIfClean`；只有工作树干净且能严格 fast-forward 时才 fetch/更新。脏、领先、分叉或网络失败会保留现有源码，绝不 `reset`、`checkout`、`clean` 或强行合并；源码真的变化后由上一个 watcher 构建。

它不使用 `SYSTEM`、管理员／最高权限、保存密码、`-Command` 或 `ihub.exe` 作为任务动作，也不会启动、结束或强制替换运行中的 iHub。启用事务完成后会立即启动两个任务，后续登录再由登录触发器恢复；当前正在运行的安装版仍必须由你从托盘自行退出后才会更新。

重复启用会做一次完整的协作交接，而不是用 `Register-ScheduledTask -Force` 覆盖仍在运行的定义：脚本先写 stop signal、注销带 ownership marker 的旧任务以阻断重启，再按精确的 `powershell.exe -File "<wrapper>"` 命令行等待所有旧 watcher／refresh 进程自然退出，包括已经不受当前 Task Scheduler 定义跟踪的 orphan。全部归零后才原子写新 wrapper、注册新任务并启动。管理事务、watcher 与 refresh 分别使用基于安装目录的全局命名互斥；即使任务定义被外部重建，也最多只有一个实例进入实际工作循环。超时或中途注册失败会保留 stop signal、回滚本轮 owned task 并拒绝形成半配置，全程不停止任何进程。可在 Windows 或 CI 中运行 `.\scripts\verify-windows-development-scripts.ps1`，检查主脚本和生成 wrapper 的语法、禁用强停命令、禁止强制覆盖任务，并从 PowerShell 7 验证 Windows PowerShell 任务对象可构造。

`-DevelopmentInstallStatus` 只在 watcher 的一次安装已经通过上述 payload SHA-256 对比后，才把 `watcherService.healthy` 报为 `true`；同一状态还会给出 `installedFingerprint`、`lastSuccessAt` 与 `lastError`。这些字段证明最近一次持久 watcher 安装的精确二进制和结果，不代表上游此刻可访问，也不代表运行中的应用会被即时替换。

检查和关闭同样是显式操作：

```powershell
.\scripts\install-dev.ps1 -DevelopmentInstallStatus
.\scripts\install-dev.ps1 -DisablePersistentDevelopmentInstall
```

关闭会先写入协作停止信号，再注销带有 iHub ownership marker 的未来登录任务，并等待所有精确 wrapper 进程在轮询／安装安全边界自行退出后才返回；它不会调用 `Stop-Process` 或 `Stop-ScheduledTask`。若有构建正在进行，等待可能长于数秒，但任务已先注销且 stop signal 会一直保留。macOS 的同类用户级 LaunchAgent 规则见下一节；两个平台都不能把“运行中的二进制立即被强制替换”伪装成已实现能力。

### 可选的 macOS 持久开发安装服务

macOS 对应的持久编排同样默认**关闭**，也不会通过应用的“开机自启”设置偷偷启用。它是给本地开发工作树使用的用户级 LaunchAgent，不是正式 Release 的 Tauri updater；平台级安装校验与状态字段仍以本节各自描述为准。先只创建受信任的 launcher marker，再明确启用：

```bash
bash ./scripts/install-dev.sh --install-launcher
bash ./scripts/install-dev.sh \
  --enable-persistent-development-install \
  --upstream-check-minutes 30 \
  --signing-key-path "$HOME/.config/ihub/tauri-updater.key"
```

启用前可先查看实际会写入的文件与 `launchctl` 动作，且不产生任何副作用：

```bash
bash ./scripts/install-dev.sh \
  --enable-persistent-development-install \
  --signing-key-path "$HOME/.config/ihub/tauri-updater.key" \
  --dry-run
```

它只在 `~/Library/LaunchAgents/` 注册两个属于**当前登录用户**的、有限权限 LaunchAgent：

- `com.neko233.ihub.development.watch` 以精确的 `/bin/bash <wrapper>` 参数启动当前工作树的 `--watch-install`；它只有在源码稳定、重新生成 updater sidecar 且你已经自行关闭 `~/Applications/iHub.app` 后，才会替换开发安装副本。
- `com.neko233.ihub.development.refresh` 以指定间隔运行 `--update-if-clean --verify-only --skip-install --skip-check`。工作树脏、领先、分叉、没有 upstream 或网络不可用时会安全跳过，绝不 reset、checkout、clean 或强制合并；真正 fast-forward 的源码变化随后由 watcher 构建。

wrapper、状态文件和限权日志都在 `~/Library/Application Support/iHub Development/`。plist 只包含 `/bin/bash` 和 wrapper 的精确路径，不包含私钥、密码或其内容；`--signing-key-path` 只允许常规本地文件路径，wrapper 才以用户权限在内存中导出该路径。它不使用 `sudo`、`open`、`sh -c`、生产 app executable action 或系统级 LaunchDaemon。若本地 updater 私钥不存在，启用会明确失败，绝不会把未签名开发包伪装成可安装版本。

若 updater 私钥已加密，在启用命令中再显式加入 `--signing-password-path /absolute/path/to/password-file`；该参数只接受现有的常规绝对文件路径。未加密私钥应省略它，密码内容不会写进 plist、marker 或日志。

查看和关闭也是显式的：

```bash
bash ./scripts/install-dev.sh --development-install-status
bash ./scripts/install-dev.sh --disable-persistent-development-install
```

关闭会先写协作 stop signal，再将**已验证为 iHub 所有**的 plist 移出未来登录发现目录；它不对运行中的 LaunchAgent 调用 `launchctl bootout`，也不结束 iHub 或构建进程。已加载的 wrapper 会在下一次轮询／构建边界读取 stop signal 后自行退出。为避免把刚加载的旧 agent 与新 agent 重叠，停止后应先确认状态，再注销并重新登录后才重新启用。开发安装目标始终是 `~/Applications/iHub.app`，不覆盖正式发行版默认的 `/Applications/iHub.app`。

应用内自动更新是签名 Release 的生产能力，不是开发机源码同步的替代品。只有填入真实 updater 公钥、配置签名私钥并发布通过校验的 `latest.json` 后，才应把它作为普通用户的“始终最新”渠道。
