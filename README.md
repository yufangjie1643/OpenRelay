# OpenRelay

个人统一 LLM API 网关。

OpenRelay is a personal unified LLM API gateway. It is now a Rust-first Windows application: one `openrelay.exe` runs the local admin panel, OpenAI-compatible proxy, SQLite usage database, and system tray manager on `http://localhost:18783`.

## 功能特性

- 统一代理入口：支持 OpenAI 兼容接口、Gemini 原生接口、`/proxy/*` 前缀以及常见无前缀兼容路径。
- 多服务商配置：在管理面板中维护服务商、模型别名、上游模型 ID、Base URL、API Key 和 User-Agent。
- 自动生成兼容配置 YAML：保存配置时同步写入本地 `openrelay-config.yaml`。
- 虚拟密钥：支持模型白名单、预算、RPM、启用状态和过期时间。
- SQLite 用量数据库：记录请求数、token、缓存 token、费用、状态码、耗时、User-Agent 和请求 ID。
- 服务商健康检查：在管理面板中检查上游连通性、鉴权、限流/余额风险、延迟和模型数量。
- 安全与备份：支持安全体检、Windows DPAPI 保护 Provider API Key、配置自动快照、脱敏导出、导入和回滚。
- Windows 日常使用：提供安装/卸载/打包脚本、开机启动开关、版本状态和 GitHub Release 更新检查。
- Windows 托盘：Rust exe 内置托盘图标，右键菜单为“打开管理面板”“重启服务”“退出 OpenRelay”。
- 静态管理面板：`public/` 由 Rust 后端直接托管，不再依赖 Node.js 服务。
- 用户数据目录：默认把配置、兼容 YAML、SQLite 数据库等运行数据保存到 `~\.openrelay`。

## 环境要求

- Windows 优先支持。
- Rust stable 工具链。

## 快速开始

发布包解压后直接运行：

```powershell
.\openrelay.exe
```

从源码构建：

```powershell
.\build.bat
```

`build.bat` 只负责编译 release 版：

```text
target\release\openrelay.exe
```

也可以直接用 Cargo 运行：

```powershell
cargo run --release
```

如果需要控制台模式并禁用托盘：

```powershell
$env:OPENRELAY_NO_TRAY = "1"
cargo run
```

启动后访问：

```text
http://localhost:18783
```

首次运行如果没有 `config.json`，服务会自动生成默认管理员账号：

```text
用户名：admin
密码：admin123
```

登录后请立刻修改管理员密码，并在长期使用时设置 `JWT_SECRET` 环境变量。

首次启动 Rust 版时，如果旧版 Node 项目目录里已有 `config.json`、`openrelay.db` 或 `usage.jsonl`，OpenRelay 会迁移到 `~\.openrelay`。已有的新数据目录配置不会被旧配置覆盖。

## 基本使用流程

1. 打开管理面板并登录。
2. 在“模型配置”中添加服务商，填写 Base URL、API Key、User-Agent 和模型映射。
3. 在“虚拟密钥”中创建调用方使用的密钥，并按需设置模型白名单、预算和 RPM。
4. 使用 OpenAI 兼容客户端或 Gemini 原生客户端请求本代理地址。
5. 在“用量统计”中查看 SQLite 记录的请求用量和成本。

## OpenAI 兼容调用

```powershell
curl.exe http://localhost:18783/v1/chat/completions `
  -H "Authorization: Bearer sk-vk-your-key" `
  -H "Content-Type: application/json" `
  -d "{\"model\":\"your-local-model\",\"messages\":[{\"role\":\"user\",\"content\":\"hello\"}]}"
```

也可以使用 master key 调用。默认 master key 来自 `general_settings.master_key`，生产或共享环境请修改它。

常用端点：

- `GET /v1/models`
- `POST /v1/chat/completions`
- `POST /v1/responses`
- `POST /v1/completions`
- `POST /v1/embeddings`
- `POST /v1/messages`
- `POST /proxy/v1/chat/completions`

所有 OpenAI 兼容端点都会统一经过 OpenRelay 的密钥校验、模型白名单、限额检查和 SQLite 用量统计。

## Gemini 原生调用

Gemini 原生兼容入口：

- `GET /gemini/v1beta/models`
- `POST /gemini/v1beta/models/{model}:generateContent`
- `POST /gemini/v1beta/models/{model}:streamGenerateContent`
- `POST /proxy/gemini/v1beta/models/{model}:generateContent`

可以用 Gemini 常见的 `?key=` 或 `x-goog-api-key` 传 OpenRelay 虚拟密钥。OpenRelay 会在转发到 Google 时替换为服务商配置里的真实 API Key：

```powershell
curl.exe "http://localhost:18783/gemini/v1beta/models/your-local-model:generateContent?key=sk-vk-your-key" `
  -H "Content-Type: application/json" `
  -d "{\"contents\":[{\"parts\":[{\"text\":\"hello\"}]}]}"
```

Gemini 原生端点同样会统一经过密钥校验、模型白名单、限额检查和 SQLite 用量统计。

## 配置和运行态文件

默认数据目录：

```text
~\.openrelay
```

运行态文件会放在这个目录下：

- `config.json`：本地真实配置，可能包含 API Key、管理员密码哈希和虚拟密钥。
- `openrelay-config.yaml`：由管理面板根据配置自动生成。
- `openrelay.db`：SQLite 用量数据库和后续运行态数据。

旧版根目录里的 `usage.jsonl` 会在首次迁移时导入 SQLite，之后不再作为主要存储。

对话记录默认不保存。需要保留对话时，在管理面板中开启对话存储并填写明确的保存目录；OpenRelay 不会默认创建或迁移 `conversations/`。

仓库中只提交模板和源码，以下文件不应提交：

- `config.json`
- `openrelay-config.yaml`
- `openrelay.db`
- `usage.jsonl`
- `conversations/`
- `target/`

可从 `config.example.json` 了解配置结构，但不要把真实密钥写入模板文件。

## Windows 构建

- `build.bat`：编译 release 版 `target\release\openrelay.exe`。
- `.\package-windows.ps1 -Zip`：生成 `dist\OpenRelay-v<version>-windows-x64` 发布目录和 zip 包。
- `.\install.ps1 -Startup`：安装到 `%LOCALAPPDATA%\OpenRelay`，并可注册开机启动。
- `.\uninstall.ps1`：移除安装目录和开机启动项；加 `-RemoveData` 会同时删除 `~\.openrelay` 数据目录。
- 发布包中直接运行 `openrelay.exe` 即可启动托盘和后端服务。

常用环境变量：

- `OPENRELAY_DATA_DIR`：覆盖默认数据目录。
- `OPENRELAY_LEGACY_ROOT`：指定旧版数据迁移来源目录。
- `OPENRELAY_STATIC_ROOT`：指定 `public/` 和 `assets/` 所在目录。
- `OPENRELAY_NO_TRAY=1`：禁用托盘，以控制台方式运行。

## 测试

```powershell
cargo test
```

当前测试覆盖配置读写、旧版数据迁移、代理路径兼容、模型解析、用量 token 提取、计费、SQLite 用量数据库、HTTP 路由、静态资源布局和 Rust 内置托盘约束。

## 项目结构

```text
src/                       Rust 后端、代理、配置、数据迁移、SQLite 数据库和托盘集成
tests/                     Rust 集成测试
public/index.html          管理面板 UI
public/login.html          登录页
assets/openrelay.ico       托盘图标
config.example.json        可提交的配置模板
build.bat                  Windows 构建脚本
```

## 安全建议

- 不要提交真实 API Key、`config.json`、`openrelay-config.yaml`、`openrelay.db` 或 `conversations/`。
- 首次启动后立即修改默认管理员密码。
- 共享或长期运行时设置强随机 `JWT_SECRET`。
- 修改默认 `general_settings.master_key`。
- 只把虚拟密钥发给调用方，避免直接暴露上游服务商密钥。
