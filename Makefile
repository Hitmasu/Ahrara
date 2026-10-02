SHELL := /bin/bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:

AHRARA_WSS_PORT ?= 8443
ARCH ?= x86_64
PLATFORM ?= linux
DOCKER_ARCH = $(if $(filter aarch64 arm64,$(shell uname -m)),arm64,amd64)
DOCKER = docker run --rm --network host --user "$(shell id -u):$(shell id -g)" -v "$(CURDIR):/work"
CARGO = $(DOCKER) --platform linux/$(DOCKER_ARCH) -w /work/cli \
	-e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/target \
	-e AHRARA_TEST_BINARY -e AHRARA_TEST_SERVER_PUBLIC_KEY \
	-e AHRARA_TEST_SERVER_URL -e AHRARA_TEST_SMTP_ADDRESS \
	-e AHRARA_UPDATE_PUBLIC_KEY -e AHRARA_SERVER_PUBLIC_KEY ahrara-builder:$(DOCKER_ARCH) cargo

.PHONY: dev-certs server cli check test openapi openapi-check e2e mcp-e2e api-e2e web-e2e e2e-static toolchain cargo

server: toolchain
	mkdir -p dist
	case "$$(uname -m)" in x86_64) target=x86_64-unknown-linux-musl ;; aarch64|arm64) target=aarch64-unknown-linux-musl ;; *) exit 1 ;; esac
	$(CARGO) build --locked --release --manifest-path /work/server/Cargo.toml --target "$$target"
	cp ".dev/target/$$target/release/ahrara-server" dist/ahrara-server

dev-certs: server
	umask 077
	install -d -m 700 .dev/run .dev/certs .dev/profile
	if ! test -f .dev/certs/server.pem || ! test -f .dev/certs/server-key.pem || ! test -f .dev/certs/ca.pem; then
		if test -e .dev/certs/server.pem || test -e .dev/certs/server-key.pem || test -e .dev/certs/ca.pem; then
			echo 'Partial development certificate set; refusing to replace files.' >&2
			exit 1
		fi
		certwork=$$(mktemp -d)
		trap 'rm -rf "$$certwork"' EXIT
		openssl req -x509 -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc -days 365 \
			-subj '/CN=Ahrara Development CA' -addext 'basicConstraints=critical,CA:TRUE' \
			-addext 'keyUsage=critical,keyCertSign,cRLSign' \
			-keyout "$$certwork/ca-key.pem" -out .dev/certs/ca.pem
		openssl req -new -newkey ec -pkeyopt ec_paramgen_curve:P-256 -noenc \
			-subj /CN=localhost -addext 'subjectAltName=DNS:localhost,IP:127.0.0.1,IP:::1' \
			-addext 'basicConstraints=critical,CA:FALSE' -addext 'extendedKeyUsage=serverAuth' \
			-keyout .dev/certs/server-key.pem -out "$$certwork/server.csr"
		openssl x509 -req -in "$$certwork/server.csr" -CA .dev/certs/ca.pem \
			-CAkey "$$certwork/ca-key.pem" -set_serial 1 -days 365 -copy_extensions copy \
			-out .dev/certs/server.pem
	fi
	if test -e .dev/certs/transport.key; then
		./dist/ahrara-server transport-public-key .dev/certs/transport.key
	else
		./dist/ahrara-server transport-keygen .dev/certs/transport.key
	fi > .dev/certs/transport-public.txt
	if ! test -e .dev/profile/config.toml; then
		pin=$$(cat .dev/certs/transport-public.txt)
		printf '%s\n' 'server_url = "wss://localhost:$(AHRARA_WSS_PORT)"' \
			"server_public_key = \"$$pin\"" 'smtp_dev_address = "localhost:2525"' \
			'tls_ca_path = "../certs/ca.pem"' 'database_path = "ahrara.db"' \
			'identity_path = "identity.key"' 'api_token_path = "api.token"' \
			'api_tls_cert_path = "../certs/server.pem"' \
			'api_tls_key_path = "../certs/server-key.pem"' > .dev/profile/config.toml
	fi

cli: DOCKER_ARCH = $(if $(filter aarch64,$(ARCH)),arm64,amd64)
cli: toolchain
	$(CARGO) build --locked --release --bin ahrara --target $(ARCH)-unknown-linux-musl
	mkdir -p dist
	cp .dev/target/$(ARCH)-unknown-linux-musl/release/ahrara dist/ahrara-linux-$(ARCH)
	cp dist/ahrara-linux-$(ARCH) dist/ahrara

.PHONY: release cross-toolchain
release: cross-toolchain
	case "$(PLATFORM)-$(ARCH)" in
		linux-x86_64|linux-aarch64) target=$(ARCH)-unknown-linux-musl; suffix= ;;
		darwin-x86_64|darwin-aarch64) target=$(ARCH)-apple-darwin; suffix= ;;
		windows-x86_64) target=x86_64-pc-windows-gnu; suffix=.exe ;;
		windows-aarch64) target=aarch64-pc-windows-gnullvm; suffix=.exe ;;
		*) echo 'Unsupported release platform/architecture.' >&2; exit 1 ;;
	esac
	$(DOCKER) --platform linux/amd64 -w /work/cli \
		-e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/cross-target \
		-e CARGO_ZIGBUILD_CACHE_DIR=/work/.dev/zigbuild -e ZIG_GLOBAL_CACHE_DIR=/work/.dev/zig \
		-e AHRARA_UPDATE_PUBLIC_KEY -e AHRARA_SERVER_PUBLIC_KEY ahrara-builder:cross \
		cargo zigbuild --locked --release --bin ahrara --target "$$target"
	mkdir -p dist
	cp ".dev/cross-target/$$target/release/ahrara$$suffix" "dist/ahrara-$(PLATFORM)-$(ARCH)$$suffix"

cross-toolchain:
	@mkdir -p .dev/cargo .dev/cross-target .dev/zigbuild .dev/zig
	docker build --platform linux/amd64 --target cross -f cli/Dockerfile.builder -t ahrara-builder:cross .

check: toolchain
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --all-targets -- -D warnings
	$(CARGO) fmt --manifest-path /work/server/Cargo.toml --all -- --check
	$(CARGO) clippy --locked --manifest-path /work/server/Cargo.toml --all-targets --all-features -- -D warnings

test: toolchain
	$(CARGO) test --locked --lib --bin ahrara --test local --test web --test update_cli --test crypto_cli --test mcp_stdio
	$(CARGO) test --locked --manifest-path third_party/mailparse/Cargo.toml
	$(CARGO) test --locked --manifest-path /work/server/Cargo.toml --all-features
	python3 -B -m unittest discover -s tests -p 'test_release_manifest.py'
	AHRARA_TEST_BINARY="$(CURDIR)/.dev/target/debug/ahrara" node --test tests/plugin.test.mjs

.PHONY: update-test
update-test: toolchain
	$(CARGO) test --locked --lib update::tests
	$(CARGO) test --locked --test update_cli
	python3 -B -m unittest discover -s tests -p 'test_release_manifest.py'

openapi: toolchain
	$(CARGO) run --locked --quiet --bin ahrara -- openapi > openapi.yaml

openapi-check: toolchain
	$(CARGO) test --locked --test local openapi_contract

e2e: server
	$(CARGO) test --locked --test e2e cli_end_to_end -- --ignored --nocapture
	$(CARGO) test --locked --test e2e reconnect_end_to_end -- --ignored --nocapture

mcp-e2e: server
	$(CARGO) test --locked --test e2e mcp_ -- --ignored --nocapture

api-e2e: server
	$(CARGO) test --locked --test e2e api_end_to_end -- --ignored --nocapture

web-e2e: server
	$(CARGO) test --locked --test e2e web_end_to_end -- --ignored --nocapture

.PHONY: forwarding-e2e
forwarding-e2e: server
	$(CARGO) test --locked --test e2e forwarding_end_to_end -- --ignored --nocapture

e2e-static: cli server
	AHRARA_TEST_BINARY=/work/dist/ahrara-linux-$(ARCH) $(CARGO) test --locked --test e2e -- --ignored --test-threads=1

toolchain:
	@mkdir -p .dev/cargo .dev/target
	if ! docker image inspect ahrara-builder:$(DOCKER_ARCH) >/dev/null 2>&1; then
		docker build --platform linux/$(DOCKER_ARCH) --target native -f cli/Dockerfile.builder -t ahrara-builder:$(DOCKER_ARCH) .
	fi

cargo: toolchain
	@$(CARGO) $(ARGS)
