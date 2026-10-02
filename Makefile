AHRARA_WSS_PORT ?= 8443
.PHONY: dev-certs dev-tools cli check test openapi openapi-check e2e mcp-e2e api-e2e e2e-static secrets-check

dev-tools:
	mkdir -p dist
	./scripts/go.sh sh -c 'CGO_ENABLED=0 go build -trimpath -o /work/dist/ahrara-dev ./cmd/ahrara-dev && CGO_ENABLED=0 go build -trimpath -o /work/dist/ahrara-server ./cmd/ahrara-server'

dev-certs: dev-tools
	./dist/ahrara-dev certs --port $(AHRARA_WSS_PORT)

cli:
	./scripts/build-linux.sh x86_64
	cp dist/ahrara-linux-x86_64 dist/ahrara

check:
	./scripts/go.sh sh -c 'test -z "$$(gofmt -l cmd internal)" && go vet ./...'
	./scripts/cargo.sh fmt --all -- --check
	./scripts/cargo.sh clippy --locked --all-targets -- -D warnings

test:
	./scripts/go.sh go test -race ./...
	./scripts/go.sh sh -c 'cd third_party/go-smtp && go test ./...'
	./scripts/cargo.sh test --locked --lib --test local
	./scripts/cargo.sh test --locked --manifest-path third_party/mailparse/Cargo.toml

openapi:
	./scripts/cargo.sh run --locked --quiet --bin ahrara -- openapi > openapi.yaml

openapi-check:
	./scripts/cargo.sh test --locked --test local openapi_contract

e2e: dev-tools
	./scripts/cargo.sh test --locked --test e2e cli_end_to_end -- --ignored --nocapture
	./scripts/cargo.sh test --locked --test e2e reconnect_end_to_end -- --ignored --nocapture

mcp-e2e: dev-tools
	./scripts/cargo.sh test --locked --test e2e mcp_end_to_end -- --ignored --nocapture

api-e2e: dev-tools
	./scripts/cargo.sh test --locked --test e2e api_end_to_end -- --ignored --nocapture

secrets-check:
	./scripts/check-secrets.sh

e2e-static: cli dev-tools
	AHRARA_TEST_BINARY=/work/dist/ahrara-linux-x86_64 ./scripts/cargo.sh test --locked --test e2e -- --ignored --test-threads=1
