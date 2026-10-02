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
- Database conversion and import validate integrity before replacing data.
  Import requires the matching Recovery Key, an offline destination, and explicit
  `--replace` to overwrite an existing inbox. Identity replacement is refused
  while an inbox exists.
- Local MCP uses HTTP without authentication by default. Host and Origin checks
  restrict browser access; any local process can use the inbox through MCP.
  Enable `--https` and `--mcp-auth` independently when needed. The API always
  requires a bearer token, and Web has a separate optional password.
  LAN mode requires HTTPS and MCP authentication. Keep the service on loopback
  or a trusted private LAN.
- Email is untrusted input, including when passed to an AI agent.

See the [user guide](docs/usage.md) for configuration and backups.
