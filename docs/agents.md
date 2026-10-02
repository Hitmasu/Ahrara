# AI agents

The repository distributes the Ahrara plugin and its shared
[skill](../skills/ahrara/SKILL.md). The plugin installs a native release when
needed, starts Ahrara, and gives the agent access to its inbox through MCP.
Node.js 20+ is required; binaries support Linux, macOS, and Windows on x64 and
ARM64. No administrator access or Rust toolchain is needed.

## Install from the repository

Use the commands for your host, then reload its plugins or start a new session.
Approve installation and MCP access through the host's normal prompts.

**Claude Code**, inside Claude:

```text
/plugin marketplace add Hitmasu/Ahrara
/plugin install ahrara@ahrara
```

**Codex**:

```sh
codex plugin marketplace add Hitmasu/Ahrara
codex plugin add ahrara@ahrara
```

**GitHub Copilot CLI**:

```sh
copilot plugin marketplace add Hitmasu/Ahrara
copilot plugin install ahrara@ahrara
```

**Gemini CLI**:

```sh
gemini extensions install https://github.com/Hitmasu/Ahrara
```

Ask the agent: “Create an Ahrara address and wait for my verification email.”
Existing compatible installations are reused. Otherwise, the launcher downloads
the platform's binary and verifies the signed release manifest and checksum.
The first launch needs internet access. If the host's startup timeout is too
short, have the agent run `node <plugin-directory>/src/plugin/ahrara.mjs --install`
and reconnect MCP.

## Other hosts

Coverage follows [Ponytail's agent adapters](https://github.com/DietrichGebert/ponytail/blob/main/docs/agent-portability.md).
Some hosts load a native plugin; others load the shared skill or project
instructions. In the latter case, the agent registers MCP when supported or
uses the same native CLI through its execution tools.

| Host | Installation |
|---|---|
| OpenCode | Clone the repository, then add the absolute path to `.opencode/plugins/ahrara.mjs` to `plugin` in `opencode.json`. The adapter adds the skill and local MCP server. |
| Antigravity CLI | `agy plugin install https://github.com/Hitmasu/Ahrara` |
| Pi | `pi install git:github.com/Hitmasu/Ahrara` |
| Hermes Agent | `hermes plugins install Hitmasu/Ahrara --enable` |
| Devin CLI | `devin plugins install Hitmasu/Ahrara` |
| Grok Build | `grok plugin install Hitmasu/Ahrara --trust`, then enable Ahrara in `/plugins`. |
| Qoder | Load `.qoder-plugin/plugin.json`, or copy `.qoder/rules/ahrara.md` into the project. |
| Swival | `swival skills add https://github.com/Hitmasu/Ahrara` |
| OpenClaw | Copy `skills/ahrara` into `~/.openclaw/skills/ahrara`. |
| Cursor | Copy `.cursor/rules/ahrara.mdc` into the project's matching directory. |
| Windsurf | Copy `.windsurf/rules/ahrara.md` into the project's matching directory. |
| Cline | Copy `.clinerules/ahrara.md` into the project's matching directory. |
| GitHub Copilot editor | Merge the Ahrara instructions into `.github/copilot-instructions.md`. |
| Kiro | Copy `.kiro/steering/ahrara.md` into the project and select that steering file. |
| Zed, CodeWhale, Amp, Jules, VS Code Codex extension | Merge the Ahrara usage paragraph from `AGENTS.md` into the project's instructions. |
| JetBrains Junie | Point Guidelines Path in Junie's project settings at the merged `AGENTS.md`. |
| Aider | Load `skills/ahrara/SKILL.md` with `/read`. |
| Other agents | Load `skills/ahrara/SKILL.md` as instructions. |

Preserve existing instructions and MCP entries. Skill-only installations ask the
agent to keep one Ahrara checkout outside the working project. Remote agents
need command execution, persistent local storage, and outbound WSS access;
their inbox lives in that environment.

## MCP command and lifetime

For a host with a generic stdio configuration, use:

```json
{
  "mcpServers": {
    "ahrara": {
      "command": "node",
      "args": ["/absolute/path/to/Ahrara/src/plugin/ahrara.mjs"]
    }
  }
}
```

Replace the absolute path with the installed checkout. For an already installed
binary, use command `ahrara` and arguments `["mcp", "--transport", "stdio"]`.
Add `--config` and an absolute configuration path for a separate profile.

Agents share one local receiver per profile, including its Web interface.
Closing one agent keeps other sessions working. The last stdio session stops
the server if agents started it; a manually started server stays running.
Identity, addresses, and received mail persist. `ahrara_stop` explicitly stops
shared reception for every interface, so it is not a session cleanup command.

Local HTTP MCP remains available as described in the [user guide](usage.md#mcp).
Stdio reuses the profile's HTTPS and authentication settings when enabled.
No certificate or token is required with the local defaults.
For local HTTPS with a private CA, include its certificate in `api_tls_cert_path`'s
PEM bundle or configure `tls_ca_path`. Certificate and hostname checks remain enabled.

## Updates and removal

Update a plugin-owned binary with `node <launcher-path> --update`. An Ahrara
installed through a package manager keeps that manager's update process.
Plugin-owned binaries live under the user data directory in `ahrara/agent`,
separately from the profile and the host's plugin cache.

Uninstall through the host or remove the copied rule and MCP entry. Removing
the plugin preserves the inbox and Identity. Stop its sessions before deleting
the binary cache. See [backups](usage.md#backups) before removing profile data.

For local development, `AHRARA_BINARY` can point to an absolute path to a native
binary with stdio support. Publishing automatic installation requires the
public keys and signed binaries described in the [release guide](releasing.md).
