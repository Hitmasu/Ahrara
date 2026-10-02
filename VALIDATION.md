# Validação local — 2026-09-26

Implementação realizada em `/home/flavio/projects/ahrara`, inicialmente vazio.
Ahrara é independente de Naeko. Foram aplicadas as instruções fornecidas pelo
usuário; não havia `AGENTS.md` local ou em diretórios ancestrais aplicáveis.
Não houve deploy, configuração DNS, infraestrutura externa, commit ou publicação.

## Ambiente e artefatos

- Host de execução: Linux x86_64; Docker Engine 28.5.1 e Compose 2.40.2.
- Builders Go 1.27.1 e Rust 1.98.1 em Docker, com imagens por digest e lockfiles.
- `dist/ahrara-linux-x86_64` e cópia `dist/ahrara`: executável release musl,
  `ELF 64-bit ... static-pie linked, stripped`; `ldd` confirmou `statically linked`.
- `dist/ahrara-server` e `dist/ahrara-dev`: executáveis locais de desenvolvimento.
- Certificados, identidades/tokens de teste, bancos e artefatos ficam fora do Git.
- Container `ahrara-server-1` confirmado **healthy**, rootfs read-only e UID/GID
  `1000:1000`. `docker diff` mostrou apenas os pontos de montagem de certificados;
  nenhum arquivo de mensagens ou aliases.

A porta 8443 já estava ocupada por um serviço existente. A validação utilizou
`AHRARA_WSS_PORT=18443`, mantendo SMTP 2525 e sem alterar o padrão 8443 do projeto.
A API usou portas loopback isoladas nos E2E; o padrão do produto continua 8787.
O container local fica disponível em `127.0.0.1:2525` e `127.0.0.1:18443`.

## Comandos executados e resultados

| Comando/verificação | Resultado observado |
|---|---|
| `make dev-certs AHRARA_WSS_PORT=18443` | CA, certificado com SANs e configuração local gerados |
| `make check` | gofmt, go vet, rustfmt e Clippy com `-D warnings` passaram |
| `make test` | Go com detector de corridas, testes Rust e suites vendorizadas passaram |
| `make openapi-check` | YAML versionado igual ao gerado pelo código |
| `AHRARA_WSS_PORT=18443 docker compose build` | Imagem Go construída |
| `AHRARA_WSS_PORT=18443 docker compose up -d --wait` | Container saudável |
| `docker compose ps` | Portas publicadas exclusivamente em loopback |
| `make cli` | Build release Linux x64 estático gerado |
| `make e2e` | CLI real; teste de reconexão também executado separadamente e na suite completa |
| `make mcp-e2e` | Processo MCP stdio, 17 tools, resources e cancelamento passaram |
| `make api-e2e` | Processo HTTPS real, bearer, CRUD, downloads, wait, SSE e rotação passaram |
| `make e2e-static` | CLI, MCP, API e reconexão usando o executável musl passaram |
| `bash -n scripts/*.sh` | Sintaxe dos scripts shell aprovada |
| `git diff --cached --check` | Sem problemas de whitespace em código próprio |
| `make secrets-check` | Gitleaks com saída redigida: nenhum segredo do projeto encontrado |

Após a última alteração do servidor, sua suite Go/race/vet, o build Docker e os
quatro E2E com o binário estático foram novamente executados com sucesso. A
suite própria contém 10 testes Go, 11 testes locais/unitários Rust e 4 E2E;
além disso, foram executados os testes upstream de go-smtp e os 76 testes
unitários/documentais de mailparse.

Gitleaks identifica uma chave privada **de fixture**, publicada no upstream de
`go-smtp`, em `server/third_party/go-smtp/client_test.go`. A exceção em
`.gitleaks.toml` é restrita à regra `private-key` nesse arquivo de teste.
Essa chave não é usada nos certificados de desenvolvimento nem no binário Go.
Os arquivos realmente gerados com segredos são ignorados e nunca impressos pelo
runner de validação. O relatório redigido fica em `.dev/gitleaks.json`.

## Evidências de aceite

| Critério | Evidência executada |
|---|---|
| Três endereços distintos, mesmo route, uma WSS | E2E CLI cria três aliases; log Go confirma `recipients=3, connections=1` |
| Recipient correto, MIME text/plain + HTML + anexo | Cada linha SQLite é filtrada por endereço; EML e anexo comparados byte a byte |
| Alias inventado rejeitado antes de streaming | O sender recebeu 550 em RCPT antes mesmo de abrir o arquivo EML de teste |
| Public key/route não autenticam sozinhos | Testes TLS/WSS rejeitam assinatura inválida e route fornecido em HELLO |
| Challenge expirado/reutilizado e conexão duplicada | Testes broker verificam rejeição, ausência de takeover e liberação após desconexão |
| Route estável em restart/recovery | E2E reinicia listener; testes Go/Rust compartilham vetor; importação mantém route |
| Aliases exclusivamente locais | Criação offline, colisão e concorrência no SQLite; broker possui somente registro por route |
| Recovery sem SQLite não recupera aliases | Teste importa Identity para outro profile e confirma lista vazia/rejeição do alias antigo |
| ACK somente após commit completo | Teste de rollback conjunto; E2E SMTP/SQLite; falha parcial de uma Identity retorna 451 |
| Limite inclusivo de 10 MiB | E2E aceita 10.485.760 bytes; rejeita excesso declarado e real sem commit adicional |
| Backpressure e encerramento | Cliente lento atinge deadline; shutdown durante DATA libera escritor; Go race passa |
| Ofertas e recipients múltiplos | Alias habilitado/rejeitado, timeout, desconexão, RSET, deduplicação, reserva 450 e BCC entre Identities |
| Respostas atrasadas | Broker ignora resposta sem waiter; falha de oferta não reutiliza a correlação na mesma transação |
| Heartbeat | Idle com ping/pong real e cancelamento do timer com relógio virtual Go |
| Ownership e reconnect | Disputa entre modos; lock preservado durante queda; API consulta SQLite offline e reconecta com mesmos aliases |
| MCP por endereço | Criação, espera filtrada, cursor, timeout, cancelamento, EML, anexos, exclusão e resources |
| API local | TLS sem CA é recusado; bearer ausente/inválido, paginação, filtros, Range, SSE, revogação e OpenAPI |

## Correções confirmadas durante a validação

- O limite exato de DATA falhava na dependência SMTP; o teste de regressão foi
  executado antes da correção e passou após a adaptação inclusiva.
- Aberturas simultâneas de um banco novo podiam falhar na transição para WAL.
  Um teste com três aberturas concorrentes reproduziu `database is locked`.
  O retry de inicialização é limitado a cinco segundos; o teste repete o cenário
  em 30 bancos isolados e passa. Não foi introduzido polling de e-mails.
- Uma oferta já encerrada não pode reutilizar sua correlação dentro da mesma
  transação. O teste de repetição falhou antes do bloqueio e passou depois.
  O remetente pode iniciar nova transação; isso não limita os aliases existentes.
- O estado da transação SMTP é capturado sob mutex para permitir Logout durante
  shutdown; as reservas são liberadas antes da resposta SMTP final.

As adaptações pequenas das dependências são documentadas em seus READMEs.
O parsing MIME possui orçamento de 32 níveis e 256 partes; excedê-lo preserva
o EML original e informa erro de parsing. Não há parser MIME próprio.

## Revisão de simplicidade e limites

Ponytail foi aplicado como revisão de simplicidade: um core Rust, migrations
SQLite diretas, constraints para unicidade, event bus em memória e lock nativo
de arquivo. Sem daemon, IPC entre processos, repositórios/serviços genéricos,
cache persistente, fila, banco cloud ou UI web. As bibliotecas existentes fazem
TLS, SMTP, WebSocket, MIME, MCP, HTTP, OpenAPI e clipboard.

Somente Linux x64 foi compilado e executado efetivamente. Os scripts de Linux
ARM64, Windows x64/ARM64 e macOS Intel/Apple Silicon foram preparados; seus builds
e testes de runtime, incluindo ACL Windows e clipboard gráfico, ainda dependem
dos respectivos hosts/SDKs. Não foi feito benchmark de capacidade de produção.

SSE anuncia apenas eventos do processo API e não garante replay. Recovery não
reconstrói aliases. Falhas parciais podem produzir duplicatas em nova tentativa
SMTP. Essas limitações são decisões explícitas da V1, documentadas no README
e no contrato de protocolo.
