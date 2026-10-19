# Ask Key for Windows

[English](README.md)

让 AI 智能体用上你的密码和 API 密钥，却拿不走它们。

Ask Key 在你的电脑上加密保存凭据。Claude Code、Codex、Cursor 等智能体只能看到每个凭据的名称和用途说明，看不到值。智能体需要用某个凭据时，会请 Ask Key 运行一条命令。你用 Windows Hello 批准后，Ask Key 启动这个程序，并把值放进它的环境变量。智能体拿到的是退出码（用命令行时还有程序输出），但不会从 Ask Key 那里拿到存储的值本身。

这是 [AskKey macOS 版](https://github.com/sudoHG/AskKey) 的 Windows 重写版，使用 Rust 和 Tauri。

## 下载

从 [Releases](https://github.com/WENGENG-boop/AskKey-Windows/releases/latest) 下载 MSI 安装包。目前是预览版，安装包没有签名，SmartScreen 会提示风险（点“更多信息”→“仍要运行”）。需要 Windows 10 22H2 或 Windows 11（x64），以及 Windows Hello（设置 PIN 即可）。

## 工作方式

- **每个凭据一个权限。** “询问”（默认）每次都要你批准；“允许”让智能体无需询问即可使用；“隐藏”让智能体看不到它。
- **审批时能看到要运行什么。** 程序、参数、工作目录，以及值会注入成哪些环境变量名。智能体自报的身份标注为“未经验证”。
- **绑定且一次性。** 一次批准只对那条命令有效一次，五分钟后过期。命令一改就需要重新批准。
- **完全本地。** 没有账号，没有云，也没有网络服务。App 和 `askkey.exe` 之间用只有你的 Windows 账户能打开的命名管道通信。

安全模型及与 macOS 版的差异见 [docs/adr/0001-windows-edition.md](docs/adr/0001-windows-edition.md)。最重要的一点：资料库密钥由 DPAPI 保护，以你的身份运行的其他程序也能读取它。

## 在智能体中使用

在 Ask Key 的“智能体”页面为你的客户端点击“连接”，或者手动配置 MCP：

```powershell
claude mcp add-json askkey '{"type":"stdio","command":"C:\\Program Files\\Ask Key\\askkey.exe","args":["mcp"]}' --scope user
```

之后智能体可以调用 `list_credentials` 和 `run`。需要看到命令输出时，用命令行：

```powershell
& "C:\Program Files\Ask Key\askkey.exe" run --wait-for-approval --credential "GitHub" --operation-id deploy-1 -- gh.exe release list
```

目标程序必须是 `.exe`。批处理文件（包括 npm 的 `.cmd` 包装脚本）会被拒绝；确实需要时，请显式写出 `cmd.exe`。

## 从源码构建

需要 Rust（stable，MSVC）、带 C++ 工作负载的 Visual Studio Build Tools，以及 WebView2。

```powershell
cargo test --workspace
cargo run -p askkey-app          # 开发版，使用独立的 AskKey\dev 资料库
.\scripts\package.ps1            # 构建正式版 App、助手和 MSI
```

## 许可证

MIT，见 [LICENSE](LICENSE) 和 [NOTICE](NOTICE)。
