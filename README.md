# LiteLLM Proxy WebUI

一个 Windows 优先的 LiteLLM / OpenAI 兼容统一代理，内置浏览器管理面板、模型配置、虚拟密钥、用量统计、对话记录和活跃连接管理。服务默认监听 `http://localhost:18783`，不需要再启动单独的 LiteLLM 进程或额外的 `4000` 端口。

## 功能特性

- 统一 OpenAI 兼容代理：支持 `/v1/*`、`/proxy/v1/*` 以及常见无前缀兼容路径。
- 多服务商配置：在管理面板中维护服务商、模型别名、上游模型 ID、Base URL、API Key 和 User-Agent。
- 自动生成 LiteLLM YAML：保存配置时同步写入本地 `litellm-config.yaml`。
- 虚拟密钥：支持按密钥设置模型白名单、预算、RPM、启用状态和过期时间。
- 用量统计：记录请求数、输入/输出 token、缓存 token、费用、状态码、耗时和 User-Agent。
- 对话记录：可选保存请求/响应，并提供阅读器、原始 JSON、流式输出解析和删除功能。
- 活跃连接管理：查看正在进行的请求，并可在管理面板中中止请求。
- Windows 启动入口：提供批处理启动脚本和可选系统托盘启动器。

## 环境要求

- Windows 环境优先支持。
- Node.js 18 或更高版本。
- npm。

## 快速开始

从仓库根目录执行：

```powershell
.\setup.ps1
.\start.bat
```

也可以直接进入 `web` 目录启动：

```powershell
cd web
npm install
npm start
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

## 基本使用流程

1. 打开管理面板并登录。
2. 在“模型配置”中添加服务商，填写 Base URL、API Key、User-Agent 和模型映射。
3. 在“虚拟密钥”中创建调用方使用的密钥，并按需设置模型白名单、预算和 RPM。
4. 使用 OpenAI 兼容客户端请求本代理地址。
5. 在“用量统计”“对话记录”“活跃连接”中查看运行情况。

## OpenAI 兼容调用

示例请求：

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

项目还保留了 MiniMax 等服务商的部分原生路径转发能力，例如语音、图像、视频和音乐生成相关端点。

## 配置和运行态文件

仓库中只提交模板和源码，以下文件为本地运行时生成或包含敏感信息，默认不会提交：

- `config.json`：本地真实配置，可能包含 API Key、管理员密码哈希和虚拟密钥。
- `litellm-config.yaml`：由管理面板根据配置自动生成。
- `usage.jsonl`：用量日志。
- `conversations/`：可选对话记录目录。
- `web/node_modules/`：Node.js 依赖。

可从 `config.example.json` 了解配置结构，但不要把真实密钥写入模板文件。

## Windows 启动脚本

- `setup.ps1`：安装 `web` 目录下的 npm 依赖。
- `start.bat`：在当前窗口启动 Web UI / API。
- `start-web.bat`：等价的 Web 服务启动脚本。
- `start-all.bat`：新开命令窗口启动服务。
- `start-tray.vbs`：兼容旧快捷方式，委托给 `start-tray.exe`。

如果需要重新构建托盘启动器，可使用 `start-tray.c` 顶部注释中的 MinGW 命令：

```powershell
windres start-tray.rc -O coff -o start-tray.res
gcc start-tray.c start-tray.res -O2 -Wall -Wextra -mwindows -municode -o start-tray.exe
```

`start-tray.exe` 是生成物，默认不提交。

## 测试

从 `web` 目录运行：

```powershell
npm test
```

也可以单独运行：

```powershell
npm run test:frontend
npm run test:proxy
```

当前测试覆盖前端静态约束、内联脚本解析、代理路径兼容、模型解析、用量 token 提取、计费、归档、对话存储路径和托盘启动约束。

## 项目结构

```text
web/server.js              Express 服务、配置迁移、代理、认证、用量和对话记录
web/public/index.html      管理面板 UI
web/public/login.html      登录页
web/package.json           Node.js 依赖和 npm scripts
config.example.json        可提交的配置模板
setup.ps1                  Windows 安装脚本
start*.bat / start*.vbs    Windows 启动脚本
assets/                    图标等静态资源
test/                      辅助分析脚本
```

## 安全建议

- 不要提交真实 API Key、`config.json`、`litellm-config.yaml`、`usage.jsonl` 或 `conversations/`。
- 首次启动后立即修改默认管理员密码。
- 共享或长期运行时设置强随机 `JWT_SECRET`。
- 修改默认 `general_settings.master_key`。
- 只把虚拟密钥发给调用方，避免直接暴露上游服务商密钥。
