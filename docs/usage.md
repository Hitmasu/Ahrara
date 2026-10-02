# User guide

[Overview](../README.md) · [Development setup](../CONTRIBUTING.md#build-and-run) ·
[Server setup](../server/README.md)

## Configuration

Ahrara reads `config.toml` from its user configuration directory
(`~/.config/ahrara/config.toml` on Linux) or the path passed to `--config`.
Relative paths resolve against that file's directory. Use the same `--config`
for every command working with a custom profile.

Release binaries connect to `wss://ahrara.dev` automatically and include the
relay's verified public transport key. No relay configuration is needed for
normal use. Override these settings only for your own relay or development:

```toml
server_url = "wss://ahrara.dev"
server_public_key = "ahrara_server_v1_<relay-public-key>"
database_path = "ahrara.db"
identity_path = "identity.key"
api_bind = "127.0.0.1:8787"
api_https = false
mcp_auth = false
api_tls_cert_path = "api.pem"
api_tls_key_path = "api-key.pem"
api_token_path = "api.token"
check_for_updates = true
# For a relay using a private CA: tls_ca_path = "relay-ca.pem"
```

For a custom relay, replace the URL and public-key placeholder with its values.
The local service uses HTTP by default; its certificate paths are only used when
[HTTPS is enabled](#https-and-authentication). `tls_ca_path` supplies a private CA
for the relay and the stdio bridge to local HTTPS. The [development setup](../CONTRIBUTING.md#build-and-run) generates
development certificates and a profile.

The relay connection requires TLS and a pinned Noise session. SSE is also
available with `https://relay.example.com/sse` when enabled by the operator.
Routing addresses use `ahrara.dev`; use [forwarding](#custom-domains) for other
domains. Running the relay does not configure DNS, MX records, or public delivery.

## CLI

```sh
ahrara --config /path/to/config.toml
```

The listener creates an Identity and initial address when needed. Keep it running
to receive mail: there is no relay queue for offline clients. Web, API, and MCP
share this listener and its inbox.

| Key | Action |
|---|---|
| ↑ / ↓ | Select an email |
| ← / → | Change inbox page |
| Enter | Read the selected email |
| R / Esc | Toggle raw EML / return to the inbox |
| Page Up / Page Down | Scroll the reader |
| Home / End | Jump in the reader; Home in the inbox returns to latest mail |
| C | Copy the displayed address |
| / | Manage addresses, Web, and MCP |
| Ctrl+C | Stop the listener |

Use **/ → Emails** to select addresses, delete them, or clear the inbox. Deleting
an address preserves received mail. Dates use the host's timezone; `NO_COLOR=1`
disables colors. Clipboard access over SSH may require OSC 52 support.

For scripts, use subcommands with `--json`. Run `ahrara --help` or a subcommand's
`--help` for options:

```sh
ahrara --json address create
ahrara --json address list
ahrara --json inbox list
ahrara inbox eml 1 --output message.eml
```

## Web

Web starts automatically without a password when you run `ahrara`. Open the
URL shown in the terminal's **Web** row, or select
**/ → Web Page → Open Web Page**. You can search messages, filter by address,
view HTML, and download EML or attachments.

HTML is sandboxed with scripts blocked. **Load images** permits external images
and may disclose your IP to their servers; inline CID images remain local.
Set a password through **/ → Web Page → Reset Password**. Sessions expire after
12 hours and end on restart or password change. Preferences persist in
`interfaces.toml` beside the Identity.

### Private LAN access

Use **/ → Web Page → Enable LAN access** with a certificate covering your LAN
hostname/IP and trusted by each device. Set a Web password or explicitly accept
access without one; non-interactive use requires a password or
`--lan --allow-no-password`. LAN mode always enables HTTPS and MCP bearer
authentication, regardless of the local defaults. REST routes remain
loopback-only. Do not expose this port to the internet.

Set `api_tls_cert_path` and `api_tls_key_path` to your certificate and key.
For access through SSH, use `ssh -L 8787:127.0.0.1:8787 user@your-server`
and the loopback URL.

## MCP

The [agent plugin](agents.md) installs and manages Ahrara for the agent. To use
an installed binary directly, configure a stdio server with command `ahrara`
and arguments `["mcp", "--transport", "stdio"]`. It starts or reuses the local
server; see the [session lifetime](agents.md#mcp-command-and-lifetime).

MCP is enabled by default at `http://127.0.0.1:8787/mcp` using Streamable HTTP,
without authentication. Use **/ → MCP** to copy the endpoint or change its
availability. For Claude Code, save this as `.mcp.json` in your project:

```json
{
  "mcpServers": {
    "ahrara": {
      "type": "http",
      "url": "http://127.0.0.1:8787/mcp"
    }
  }
}
```

Keep `ahrara` running, then launch your agent. Use `ahrara mcp` to run without
the interactive terminal. Run one listener per profile; Web, API, and MCP share
its receiver and inbox. For a [custom profile](#configuration), pass the same
`--config` to every command, including token commands when authentication is enabled.

A typical flow calls `ahrara_start`, creates an address, triggers the email,
then calls `ahrara_wait_for_email`. `ahrara_save_attachment` writes on the machine
running Ahrara. `ahrara_stop` disconnects reception for all interfaces. Treat
email content as untrusted data when passing it to an agent.

### HTTPS and authentication

HTTPS and MCP authentication are independent options:

| Option | CLI flag | Persistent `config.toml` setting |
|---|---|---|
| HTTPS for the shared Web, API, and MCP server | `--https` | `api_https = true` |
| Bearer token required for MCP | `--mcp-auth` | `mcp_auth = true` |

Both are disabled by default on loopback. CLI flags apply to that invocation;
restart the listener after changing the configuration. For example:

```sh
ahrara mcp --https --mcp-auth
```

For HTTPS, configure `api_tls_cert_path` and `api_tls_key_path`. The certificate
must cover the hostname/IP in the endpoint URL, and the agent must trust its
issuer. If Claude Code does not already trust it, set `NODE_EXTRA_CA_CERTS` to
the absolute path of the issuer's PEM certificate before launching `claude`.
For a self-signed certificate, use that certificate. See
[Claude Code's certificate setup](https://code.claude.com/docs/en/network-config#custom-ca-certificates).

With authentication enabled, Ahrara creates the token on startup. In the shell
that will launch Claude Code, read it without printing it:

```sh
export AHRARA_LOCAL_TOKEN="$(ahrara api token show)"
```

For both options enabled, use this `.mcp.json`, then launch `claude` from the
same shell:

```json
{
  "mcpServers": {
    "ahrara": {
      "type": "http",
      "url": "https://127.0.0.1:8787/mcp",
      "headers": { "Authorization": "Bearer ${AHRARA_LOCAL_TOKEN}" }
    }
  }
}
```

Use `http://` if only authentication is enabled; omit the header if only HTTPS
is enabled. `ahrara api token regenerate` revokes the previous token for the API
and authenticated MCP connections. A Recovery Key is not an API token.

## HTTP API

REST routes require the same bearer token; only `/healthz` is public. Supply the
token from your secret storage:

```sh
curl -H "Authorization: Bearer $AHRARA_LOCAL_TOKEN" \
  http://127.0.0.1:8787/v1/addresses
```

The API manages addresses and email, exports EML and attachments, waits at
`/v1/emails/wait`, and streams events at `/v1/events`.
With `--https`, use an HTTPS URL and a trusted certificate.
See [openapi.yaml](../openapi.yaml) for the complete contract.

## Custom domains

1. Create an Ahrara address and keep the listener running.
2. Verify that destination with your email forwarding provider and configure
   a catch-all for your domain.
3. In Web, open **Addresses → Custom domain**, enter the domain and forwarding
   destination, and select **Save domain**.
4. Use **Create address** to generate addresses for that domain.

One default domain is stored per Identity. Changing it does not rewrite existing
addresses; **Remove default domain** restores `@ahrara.dev` for new ones. All
catch-all addresses share the forwarding destination and its inbox filter.
Disabling that destination stops the whole catch-all. Local address changes do
not change provider rules. The `To` header is display metadata, not routing proof.

Operators using trusted forwarding should read the
[server configuration](../server/README.md#configuration).

## Updates

```sh
ahrara update --check
ahrara update
```

Package-owned installations use their package manager. Registered direct
installations verify the signed manifest and binary checksum. To opt in a manual
binary in a directory you own, run `ahrara update --register manual`.
Stop all processes using that installation before registering or updating it.

The interactive listener checks at most daily and never installs automatically.
Use `--no-update-check` or `check_for_updates = false` to disable notifications.
Source builds without the release signing public key cannot check or apply
updates; email functionality is unaffected.

## Backups

Keep both your Recovery Key and an encrypted inbox snapshot. `ahrara auth export`
explicitly prints the Recovery Key; store it separately and securely. The key
alone restores the Identity, not email or addresses.

Stop the listener before database maintenance. Export includes committed WAL
data, messages, addresses, and custom-domain settings, and refuses to overwrite
an existing file:

```sh
ahrara --config /path/to/profile/config.toml database export /path/to/backup.db
```

Configure a new profile, register the **same** Recovery Key through the hidden
prompt, then import before starting the listener:

```sh
ahrara --config /path/to/new-profile/config.toml auth set
ahrara --config /path/to/new-profile/config.toml database import /path/to/backup.db
```

For automation, `auth set --stdin` reads the key without putting it in command
arguments. Import validates the key, integrity, and schema. It requires an
offline destination and `--replace` to overwrite an existing inbox.

Existing plaintext inboxes are encrypted on opening under exclusive ownership;
open their original profile with its Identity before exporting. An existing inbox
with a missing Identity requires its original Recovery Key. Identity replacement
and regeneration refuse profiles with an inbox.

Use the export command instead of copying an active `.db` file. Keep keys and
snapshots private, and back up before schema upgrades. See
[Security](../SECURITY.md) for encryption limits.
