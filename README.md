# Ask Key for Windows

[简体中文](README.zh-CN.md)

Give AI agents your passwords and API keys without giving them away.

Ask Key keeps credentials encrypted on your PC. Agents such as Claude Code, Codex and Cursor can see each credential's name and its intended use. They cannot read the values. When an agent needs one, it asks Ask Key to run a command. You approve the request with Windows Hello, and Ask Key starts the program with the value in its environment. The agent gets the exit status, and with the CLI, the program's output. It never gets the stored value from Ask Key itself.

This is a Windows reimplementation of [AskKey for macOS](https://github.com/sudoHG/AskKey) in Rust and Tauri.

## How it works

- **One permission per credential.** *Ask* (the default) needs your approval every time. *Allow* lets agents use it without asking. *Hidden* keeps agents from seeing it.
- **Approvals show what will run.** You see the program, its arguments, the working directory, and the environment variable names the values will arrive as. The agent's own description of itself is marked unverified.
- **Bound and single use.** An approval covers that exact command once and expires after five minutes. If the command changes, it needs a new approval.
- **Local only.** There is no account, no cloud and no network service. The App and the `askkey.exe` helper talk over a named pipe that only your Windows account can open.

Read [docs/adr/0001-windows-edition.md](docs/adr/0001-windows-edition.md) for the security model and how it differs from macOS. The most important difference: the library key is protected with DPAPI. Other programs running as your user can read it.

## Use it from an agent

Open **Agents** in Ask Key and click **Connect** for your client. Or configure the MCP server yourself:

```powershell
claude mcp add-json askkey '{"type":"stdio","command":"C:\\Program Files\\Ask Key\\askkey.exe","args":["mcp"]}' --scope user
```

Agents can then call `list_credentials` and `run`. To see a command's output, an agent uses the CLI:

```powershell
& "C:\Program Files\Ask Key\askkey.exe" run --wait-for-approval --credential "GitHub" --operation-id deploy-1 -- gh.exe release list
```

The target must be an `.exe`. Batch files such as npm `.cmd` shims are refused. Name `cmd.exe` explicitly if you need one.

## Build from source

You need Rust (stable, MSVC), Visual Studio Build Tools with the C++ workload, and WebView2.

```powershell
cargo test --workspace
cargo run -p askkey-app          # development build, separate AskKey\dev library
.\scripts\package.ps1            # release App, helper and MSI
```

## License

MIT. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
