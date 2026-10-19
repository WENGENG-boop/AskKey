# AGENTS.md

Rules for every agent working in this repository. `CLAUDE.md` only imports this file.

## Project

Ask Key for Windows stores credentials locally and lets AI agents use them only through the restricted `askkey.exe` helper and a Broker inside the App. Each credential has one agent permission: Allow, Ask (default) or Hidden. Agents never receive stored values in Broker responses; approved values go only into the environment of the target process the Broker launches.

It is a Rust and Tauri 2 reimplementation of [AskKey for macOS](https://github.com/sudoHG/AskKey). The product rules of that project's ADRs 0001-0003, 0008 and 0010 apply here; [docs/adr/0001-windows-edition.md](docs/adr/0001-windows-edition.md) records how they map to Windows and where Windows forces a weaker or different guarantee. Read it before changing the Broker, vault or helper.

## Build and test

Requirements: Windows 10 22H2 or 11, Rust stable (MSVC toolchain), Visual Studio Build Tools with the C++ workload and a Windows SDK, WebView2 runtime.

```powershell
cargo build                                  # all crates, debug (development namespace)
cargo test --workspace                       # unit and integration tests
cargo test -p askkey-broker serves_over      # one test by name filter
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
python scripts/check_module_deps.py          # crate dependency graph
python scripts/check_hygiene.py              # file size, paths, names
cargo run -p askkey-app                      # development App (AskKey\dev namespace)
```

`scripts/package.ps1` builds the release App and helper and the MSI. Release builds use the production namespace; never run one against real data from an agent session.

Tests use real DPAPI, named pipes and job objects, so they only run on Windows. Several spawn `cmd.exe`, `where.exe` or `ping.exe` from `System32`.

## Architecture

| Crate | Responsibility | May depend on |
|---|---|---|
| `askkey-protocol` | Wire types, length-prefixed JSON framing, limits, approval digest, inherited-environment allowlist, pipe name | — |
| `askkey-system` | All Win32 `unsafe`: DPAPI, private DACLs, user SID, pipe peer PIDs, handle duplication, job-object spawn, command-line quoting, executable resolution | — |
| `askkey-vault` | SQLite storage, AES-256-GCM fields, HMAC record authentication, DPAPI-wrapped vault key, fail-closed bootstrap, access records | system |
| `askkey-broker` | Approval state machine, `runtime.run`, named-pipe server. Talks to storage only through the `CredentialProvider` trait | protocol, system |
| `askkey-helper` (`askkey.exe`) | CLI (`run`, `health`, `version`, `open`), MCP stdio server, server-identity check | protocol, system |
| `app` (`askkey-app`) | Tauri App: management UI, approvals, Windows Hello, tray, client setup; wires the vault into the Broker | all |

The helper must never depend on `askkey-vault` or `askkey-broker`. `scripts/check_module_deps.py` enforces the graph.

Request flow: helper connects to `\\.\pipe\askkey-broker[-dev]-<SID>` (DACL: current user only; remote clients rejected; first-instance flag) and checks that the server process is the App next to it → Broker resolves the named credentials (Hidden and missing look identical) → Ask creates a ticket bound to the request digest and the App shows it → an identical retry after approval consumes the ticket once → final launch check re-resolves everything → values are decrypted and the target starts suspended in a kill-on-close job with only its three stream handles inherited. The CLI passes anonymous-pipe handles that the Broker duplicates from the helper process; MCP runs get `NUL`.

## Safety boundaries

- Never read or write the production library `%LOCALAPPDATA%\AskKey` (outside `dev`) or use the production DPAPI namespace `com.sudohg.askkey.vault`. Debug builds and tests use `%LOCALAPPDATA%\AskKey\dev`, `com.sudohg.askkey.vault.dev` or temporary directories.
- Synthetic values only in tests and examples.
- Do not install anything into `C:\Program Files` or change the user's agent-client configuration from an agent session; client-setup code is tested against temporary files.
- Do not weaken a guarantee silently. If Windows cannot provide something the macOS edition does, record it in the ADR and the UI.
- `ASKKEY_TEST_AUTH` and `ASKKEY_BROKER_PIPE` are honoured only in debug builds. Keep it that way.

## Hygiene

- Code, comments, docs and commit messages in English. UI strings live in `app/ui/i18n.js` (English and Simplified Chinese).
- No absolute local paths in committed files; tests use temporary directories.
- Source files stay under 600 lines.
- No new third-party crates without recording why in the PR.
