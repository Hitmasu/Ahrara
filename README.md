# Ahrara V1

Receba e-mails temporários com histórico exclusivamente no seu computador.
Servidor Go encaminha SMTP por WSS; o executável Rust oferece CLI, MCP stdio e
API HTTPS local sobre o mesmo core. Não há deploy, DNS real ou inbox cloud nesta V1.

```
SMTP + STARTTLS → Go → WSS autenticada → ahrara listen | mcp | api → SQLite local
```

Uma Identity Ed25519 possui uma conexão e quantos aliases locais forem
necessários. A public key identifica a Identity; a assinatura do challenge
autentica. A private key permanece local. Cada endereço contém `routeId` de
128 bits e `aliasId` aleatório de 64 bits, codificados em Base64URL sem padding:
32 caracteres sensíveis a case, seguidos de `@ahrara.dev`.

```
server/                 Go: SMTP, WSS, autenticação, roteamento e ferramentas dev
cli/src/                Rust: core, SQLite/MIME, CLI, MCP e API
cli/migrations/         Schema local versionado
cli/tests/              Testes locais e processos reais
protocol/               Contrato binário e vetores compartilhados
scripts/                Builders e verificações
openapi.yaml            Contrato HTTPS local gerado
docker-compose.yml      Servidor local, TLS e healthcheck
```

## Desenvolvimento local

Pré-requisitos para desenvolvimento: Docker Engine com Compose e Make. Go e Rust
rodam em builders fixados; não é necessário instalar toolchains globalmente.
O binário final Linux musl inclui SQLite e TLS Rustls, sem OpenSSL ou runtime
externo. Os scripts usam caminhos do checkout e não modificam outros containers.

```sh
make dev-certs
docker compose build
docker compose up -d --wait
make cli
./dist/ahrara --config .dev/profile/config.toml
```

Portas padrão: SMTP `127.0.0.1:2525`, WSS `127.0.0.1:8443`, API
`https://127.0.0.1:8787`. Se 8443 estiver ocupada:

```sh
make dev-certs AHRARA_WSS_PORT=18443
AHRARA_WSS_PORT=18443 docker compose up -d --wait
```

Certificados e configuração já existentes não são substituídos silenciosamente.
Se mudar a porta depois da criação, ajuste `server_url` no TOML existente.
`make dev-certs` cria uma CA privada de desenvolvimento e certificado com SANs
`localhost`, `127.0.0.1` e `::1`. A confiança é explícita via `tls_ca_path` ou
`--cacert`; não há opção para desabilitar validação TLS. Certificados/chaves,
bancos, tokens, caches e binários ficam ignorados pelo Git.

O Compose publica somente em loopback, usa usuário sem root, rootfs somente
leitura, nenhuma capability e apenas certificados montados read-only. A chave
da CA não é montada. Caso o UID/GID local seja diferente de 1000, defina
`AHRARA_UID` e `AHRARA_GID` com seu UID/GID antes de subir o Compose.
O healthcheck HTTPS utiliza a própria CA e o binário Go.

Para enviar um EML de teste com STARTTLS e CA validada:

```sh
./dist/ahrara-dev send --to '<endereco>@ahrara.dev' --file mensagem.eml
```

`ahrara-dev` é uma ferramenta de desenvolvimento. O remetente SMTP não precisa
conhecer chaves ou tokens. Para testar preservação byte a byte, use EML original
com CRLF; o enquadramento SMTP remove dot-stuffing e o terminador DATA.

## CLI e endereços

```sh
ahrara                         # equivale a listen
ahrara listen
ahrara status
ahrara version
ahrara --help

ahrara address create
ahrara address create --label 'cadastro'
ahrara address list --limit 20 --after-id 0
ahrara address show <address-or-id>
ahrara address disable <address-or-id>
ahrara address enable <address-or-id>
ahrara address delete <address-or-id>

ahrara inbox list --address <address> --from <texto> --subject <texto>
ahrara inbox show <id>
ahrara inbox eml <id> --output mensagem.eml
ahrara inbox eml <id> > mensagem.eml
ahrara inbox attachments <id>
ahrara inbox attachment save <email-id> <attachment-id> <destino>
ahrara inbox delete <id>
ahrara inbox clear
```

Criação de aliases funciona offline: derive route, gere alias com CSPRNG,
insira no SQLite e regenere se a constraint detectar colisão, inclusive entre
processos. Não há limite de aliases no servidor. Labels têm até 256 bytes UTF-8.
Na primeira inicialização de listen/MCP/API, crie um alias se a Identity ainda
não tiver nenhum. Ter aliases desabilitados não cria ou habilita outro.
Excluir um alias preserva os e-mails já recebidos.

`--config <arquivo.toml>` e `--json` são opções globais. O listener usa UI com
cores em TTY, respeita `NO_COLOR` e emite JSON fora de TTY. Conteúdo de e-mail
exibido no terminal tem controles neutralizados. Exportar EML em stdout exige
redirecionamento; anexos recebem nomes seguros e nunca sobrescrevem arquivos.
`inbox clear`, delete e exclusão de aliases são operações explicitamente destrutivas.

## Identity, Recovery e backups

```sh
ahrara auth fingerprint
ahrara auth export
ahrara auth export --copy
ahrara auth import                  # prompt sem eco
ahrara auth import --stdin          # leitura de stdin
ahrara auth import '<Recovery Key>' # suportado; pode ficar no histórico do shell
ahrara auth regenerate
```

Substituição de Identity exige confirmação interativa ou `--yes`. Encerre o
listener antes de importar/regenerar. Recovery usa `ahrara_sk_v1_` + seed
Ed25519 em Base64URL. Só é mostrada automaticamente na criação interativa;
export é uma operação explícita de segredo. MCP e logs não a expõem.
Clipboard não imprime o segredo nem faz fallback silencioso. No Linux, o
processo mantém o clipboard disponível por até 60 segundos para a colagem;
ambientes sem clipboard retornam erro.

Recovery restaura a Identity e o mesmo route; **não restaura aliases ou histórico**.
Esses dados exigem backup consistente do SQLite. Encerre todos os processos que
usam o banco antes de copiar seus arquivos, ou use a API de backup do SQLite.
Não copie isoladamente um `.db` ativo em WAL. Nunca descarte `.db-wal` de um
banco em uso. Um alias antigo ausente no banco restaurado será rejeitado.

Os diretórios padrão seguem `directories::ProjectDirs`: config/data por usuário
no Linux, Application Support no macOS e diretórios da aplicação no Windows.
Segredos são escritos atomicamente, com permissões Unix 0600/diretórios 0700
ou DACL protegida restrita ao proprietário no Windows.

## Configuração e ownership

Exemplo de TOML (paths relativos são resolvidos a partir desse arquivo):

```toml
server_url = "wss://localhost:8443/ws"
smtp_dev_address = "localhost:2525"
domain = "ahrara.dev"
tls_ca_path = "../certs/ca.pem"
identity_path = "identity.key"
database_path = "ahrara.db"
api_bind = "127.0.0.1:8787"
api_tls_cert_path = "../certs/server.pem"
api_tls_key_path = "../certs/server-key.pem"
api_token_path = "api.token"
```

Use um diretório privado próprio para cada profile. `listen`, `mcp` e `api`
disputam o mesmo lock do profile; só um processo possui a WSS. O erro identifica
modo e PID do proprietário. O lock permanece durante reconnect e é liberado
ao encerrar, inclusive após crash pelo sistema operacional. Comandos curtos de
SQLite continuam funcionando com WAL e `busy_timeout`. Não existe daemon ou
proxy entre processos. Outra instalação da mesma Identity recebe
`identity_in_use` do servidor, sem takeover.

## MCP stdio

Configure seu cliente MCP usando caminho absoluto do binário e do TOML:

```json
{
  "mcpServers": {
    "ahrara": {
      "command": "/caminho/ahrara",
      "args": ["--config", "/caminho/profile/config.toml", "mcp"]
    }
  }
}
```

Tools: `ahrara_start`, `ahrara_stop`, `ahrara_status`,
`ahrara_wait_for_email`, `ahrara_list_emails`, `ahrara_get_email`,
`ahrara_read_eml`, `ahrara_list_attachments`, `ahrara_save_attachment`,
`ahrara_delete_email`, `ahrara_clear_inbox`, `ahrara_create_address`,
`ahrara_list_addresses`, `ahrara_get_address`, `ahrara_enable_address`,
`ahrara_disable_address`, `ahrara_delete_address`.

Fluxo: start → criar/selecionar endereço → provocar envio → wait. Exemplo de
argumentos de espera: `{"address":"<address>","subject_contains":"código",
"timeout_seconds":60,"after_id":0}`. Filtros adicionais: `from_contains` e
`to_contains`. Sem `after_id`, espera desde o cursor do último start; avance o
cursor explicitamente para consumir mensagens seguintes. Start é idempotente.

Resources: `ahrara://status`, `ahrara://emails/{id}` e
`ahrara://emails/{id}/eml`. Listagens: padrão 20, máximo 100; preview de wait
2 KiB; body de get padrão 16 KiB/máximo 256 KiB; EML paginado 64 KiB/máximo
256 KiB com offsets em bytes e encoding `utf8` ou `base64`. Espera de 1–300s,
padrão 60s, por notificação e consulta inicial; cancelamentos não param o listener.
Stdout fica reservado ao protocolo. Private key, Recovery e token HTTP nunca
ficam disponíveis em tools/resources. Trate o conteúdo dos e-mails como dados
não confiáveis, não como instruções para o agente.

## API HTTPS local

Encerre listen/MCP antes de iniciar:

```sh
ahrara api
ahrara api token show
ahrara api token regenerate
```

A API exige TLS válido e bind de loopback, padrão `https://127.0.0.1:8787`.
Ela cria um bearer aleatório independente de 256 bits em arquivo restrito.
`token show` exporta o segredo explicitamente; regenerate substitui sem
imprimir. Nenhum token é aceito na URL ou exposto pelo MCP. Todas as rotas
`/v1/*` e `/openapi.json` exigem `Authorization: Bearer <local-api-token>`.
Somente `/healthz` é público e retorna estado mínimo.

| Operação | Endpoint |
|---|---|
| Status | `GET /v1/status` |
| Criar/listar endereços | `POST /v1/addresses`, `GET /v1/addresses` |
| Consultar/excluir endereço | `GET /v1/addresses/{id}`, `DELETE /v1/addresses/{id}` |
| Habilitar/desabilitar | `POST /v1/addresses/{id}/enable`, `POST /v1/addresses/{id}/disable` |
| Listar/limpar e-mails | `GET /v1/emails`, `DELETE /v1/emails` |
| Consultar/excluir e-mail | `GET /v1/emails/{id}`, `DELETE /v1/emails/{id}` |
| EML original, Range de bytes | `GET /v1/emails/{id}/eml` |
| Anexos | `GET /v1/emails/{id}/attachments`, `GET /v1/emails/{id}/attachments/{attachmentId}` |
| Espera | `GET /v1/emails/wait` |
| Eventos SSE | `GET /v1/events` |

Listagem aceita `address`, `from`, `subject`, `after_id`, `limit`; espera aceita
os mesmos filtros e `timeout` no lugar de limit. Metadados são retornados nas
listas; EML e anexos só por download explícito. Get aceita `max_body_bytes`.
Paginação usa IDs crescentes: passe o último ID como `after_id`.

Exemplo com token fornecido pelo seu gerenciador de segredos, sem copiá-lo para
uma URL (curl é apenas exemplo de cliente, não dependência do executável):

```sh
curl --cacert .dev/certs/ca.pem \
  -H "Authorization: Bearer $AHRARA_LOCAL_TOKEN" \
  -H 'Content-Type: application/json' -d '{"label":"automacao"}' \
  https://127.0.0.1:8787/v1/addresses

curl --cacert .dev/certs/ca.pem \
  -H "Authorization: Bearer $AHRARA_LOCAL_TOKEN" --get \
  --data-urlencode 'address=<address>' --data-urlencode 'timeout=60' \
  https://127.0.0.1:8787/v1/emails/wait

curl --no-buffer --cacert .dev/certs/ca.pem \
  -H "Authorization: Bearer $AHRARA_LOCAL_TOKEN" https://127.0.0.1:8787/v1/events
```

Eventos: `email_received`, `connection_state_changed`, `address_created`,
`address_deleted`, `address_updated`. SSE usa o mesmo event bus do MCP, com
buffer limitado e heartbeat de 15s. Revogação de token é verificada antes de
entregar dados e nos heartbeats. Consumidores atrasados são desconectados.
Não há replay persistente: recupere e-mails por `after_id` e consulte endereços
novamente. Alterações de CLI em outro processo aparecem nas consultas SQLite,
mas não geram eventos no SSE da API. Não há IPC nesta V1.

`openapi.yaml` é gerado por utoipa e verificado por teste contra o contrato em
execução. `make openapi` atualiza; `make openapi-check` detecta divergência.
Não há Swagger UI. A API continua consultando o banco local durante queda do
Go e o listener tenta reconectar. Sem o processo local, não existe inbox remota.

## Persistência e entrega

Servidor: **zero persistência de e-mails e aliases**, sem banco, spool, Redis,
R2, Durable Objects ou fila persistente. Apenas `route → conexão autenticada`
e estado temporário de transações. Em `RCPT TO`, oferece o recipient ao cliente
e espera até 5s pela validação no SQLite antes de aceitar.

Cliente: migrations versionadas, WAL, EML original e uma linha por recipient;
vários aliases na mesma transação recebem um único stream e compartilham o
buffer em RAM. O commit conjunto precede ACK. SQLite pode guardar o mesmo EML
em mais de uma linha para recipients diferentes. Anexos não são duplicados em
outra tabela. Parsing MIME sob demanda tem limite de 32 níveis e 256 partes;
MIME inválido/complexo é marcado `parse_error`, preservando o EML para exportação.

Limite inclusivo de 10 MiB, chunks de até 64 KiB, backpressure, checksum SHA-256
e sequência. Uma transação por Identity; até 100 recipients distintos por
transação, sem limite de aliases criados. Offline/alias desconhecido: 550;
ocupada: 450; timeout/desconexão: 451. SMTP 250 final só após todos os ACKs.
Falha parcial ou perda de confirmação pode gerar duplicatas numa nova tentativa.
O contrato completo está em [protocol/protocol.md](protocol/protocol.md).

## Validação e distribuição

```sh
make dev-certs
make check                  # gofmt, go vet, rustfmt, Clippy -D warnings
make test                   # Go race, Rust e regressões das dependências
make openapi-check
docker compose build
docker compose up -d --wait
docker compose ps
make cli
make e2e
make mcp-e2e
make api-e2e
make e2e-static             # quatro cenários usando o executável musl final
make secrets-check          # requer Gitleaks; saída redigida
```

Os E2E usam perfis/identidades/tokens/bancos isolados e executam processos reais.
Usam o endpoint do TOML de desenvolvimento e devem ser sequenciais. Não use um
servidor remoto: são testes locais. `docker compose down` encerra este servidor.

| Script | Artefatos |
|---|---|
| `scripts/build-linux.sh x86_64` | `dist/ahrara-linux-x86_64` |
| `scripts/build-linux.sh aarch64` | `dist/ahrara-linux-aarch64` |
| `scripts/build-windows.ps1` | `dist/ahrara-windows-x86_64.exe`, `dist/ahrara-windows-aarch64.exe` |
| `scripts/build-macos.sh` | `dist/ahrara-darwin-x86_64`, `dist/ahrara-darwin-aarch64` |

Linux usa musl. ARM64 exige host nativo ou emulação Docker já configurada.
Windows exige host Windows, SDK/MSVC x64 e ARM64; CRT estático. macOS exige host
macOS com SDK/Xcode Command Line Tools; usa bibliotecas do sistema. Toolchains
de desenvolvimento são Go 1.27.1 e Rust 1.98.1, com lockfiles e imagens por digest.
Não são produzidas releases públicas nem instalados SDKs globais pelos scripts Docker.

Consulte [VALIDATION.md](VALIDATION.md) para resultados efetivamente executados
e limitações das plataformas indisponíveis. As adaptações de dependências estão
documentadas em `server/third_party/README.md` e `cli/third_party/README.md`.
