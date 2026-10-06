# Security

## Reporting a vulnerability

Report vulnerabilities privately through
[GitHub Security Advisories](https://github.com/Hitmasu/Ahrara/security/advisories).
If **Report a vulnerability** is unavailable, open an issue requesting a private
contact without disclosing the vulnerability. Include the affected version,
reproduction steps using synthetic data, and potential impact in your private report.
Never publish real email, Recovery Keys, tokens, or credentials.

## Security boundaries

- The relay processes SMTP email in transit and does not persist an inbox. TLS
  and a pinned Noise session protect the relay-to-client protocol, including
  through a TLS-terminating proxy. Obtain the relay's public transport key through
  an independent trusted channel. SMTP senders, forwarding providers, and the
  relay can still read email; this is not sender-to-recipient encryption.
- SQLCipher encrypts the local inbox, WAL, and exported snapshots with a key
  derived from the Recovery Identity. Anyone with the Identity file or Recovery
  Key and the database can decrypt it. Runtime memory, earlier plaintext copies,
  exported EML, and attachments are outside this protection.
- Database import validates integrity before replacing data. It requires the matching Recovery Key, an offline destination, and explicit
  `--replace` to overwrite an existing inbox. Identity replacement is refused
  while an inbox exists.
- Local MCP uses HTTP and requires the local API bearer token by default, even on
  loopback; the stdio bridge reads the token from the profile. Setting
  `mcp_auth = false` lets any local process use the inbox through MCP. Host and
  Origin checks restrict browser access. Enable `--https` when needed. The HTTP
  API is disabled by default and always requires the bearer token; Web has a
  separate optional password. LAN mode requires HTTPS and MCP authentication.
  Keep the service on loopback or a trusted private LAN.
- MCP saves attachments only into the `attachments` directory next to the
  profile database, never overwriting existing files.
- Email is untrusted input, including when passed to an AI agent.

See the [user guide](docs/usage.md) for configuration and backups.
