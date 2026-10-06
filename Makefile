SHELL := /bin/bash
.SHELLFLAGS := -euo pipefail -c
.ONESHELL:

AHRARA_WSS_PORT ?= 8443
ARCH ?= x86_64
PLATFORM ?= linux
DOCKER_ARCH = $(if $(filter aarch64 arm64,$(shell uname -m)),arm64,amd64)
DOCKER = docker run --rm --network host --user "$(shell id -u):$(shell id -g)" -v "$(CURDIR):/work"
CARGO = $(DOCKER) --platform linux/$(DOCKER_ARCH) -w /work/src/cli \
	-e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/target \
	-e AHRARA_TEST_BINARY -e AHRARA_TEST_SERVER_PUBLIC_KEY \
	-e AHRARA_TEST_SERVER_URL -e AHRARA_TEST_SMTP_ADDRESS \
	-e AHRARA_UPDATE_PUBLIC_KEY -e AHRARA_SERVER_PUBLIC_KEY ahrara-builder:$(DOCKER_ARCH) cargo

.PHONY: dev-certs server cli check test client-lint client-test openapi openapi-check e2e mcp-e2e api-e2e web-e2e toolchain cargo

server: toolchain
	mkdir -p dist
	case "$$(uname -m)" in x86_64) target=x86_64-unknown-linux-musl ;; aarch64|arm64) target=aarch64-unknown-linux-musl ;; *) exit 1 ;; esac
	$(CARGO) build --locked --release --manifest-path /work/src/server/Cargo.toml --target "$$target"
	install -m755 ".dev/target/$$target/release/ahrara-server" dist/ahrara-server

dev-certs: server
	docker run --rm --network host -i --user "$$(id -u):$$(id -g)" -v "$$PWD:/work" -w /work -e AHRARA_WSS_PORT="$$AHRARA_WSS_PORT" ahrara-builder:$$SDK_DOCKER_ARCH bash -s <<'AHRARA_CERTS'
	set -euo pipefail
	# Runs inside the repository builder container.
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
		printf '%s\n' "server_url = \"wss://localhost:$${AHRARA_WSS_PORT:-8443}\"" \
			"server_public_key = \"$$pin\"" 'smtp_dev_address = "localhost:2525"' \
			'tls_ca_path = "../certs/ca.pem"' 'database_path = "ahrara.db"' \
			'identity_path = "identity.key"' 'api_token_path = "api.token"' \
			'api_tls_cert_path = "../certs/server.pem"' \
			'api_tls_key_path = "../certs/server-key.pem"' > .dev/profile/config.toml
	fi
	AHRARA_CERTS

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
	$(DOCKER) --platform linux/amd64 -w /work/src/cli \
		-e CARGO_HOME=/work/.dev/cargo -e CARGO_TARGET_DIR=/work/.dev/cross-target \
		-e CARGO_ZIGBUILD_CACHE_DIR=/work/.dev/zigbuild -e ZIG_GLOBAL_CACHE_DIR=/work/.dev/zig \
		-e AHRARA_UPDATE_PUBLIC_KEY -e AHRARA_SERVER_PUBLIC_KEY ahrara-builder:cross \
		cargo zigbuild --locked --release --bin ahrara --target "$$target"
	mkdir -p dist
	cp ".dev/cross-target/$$target/release/ahrara$$suffix" "dist/ahrara-$(PLATFORM)-$(ARCH)$$suffix"

cross-toolchain:
	@mkdir -p .dev/cargo .dev/cross-target .dev/zigbuild .dev/zig
	docker build --platform linux/amd64 --target cross -f src/cli/Dockerfile.builder -t ahrara-builder:cross .

check: toolchain client-lint
	$(CARGO) fmt --manifest-path /work/src/core/Cargo.toml --all -- --check
	$(CARGO) fmt --manifest-path /work/src/native/Cargo.toml --all -- --check
	$(CARGO) clippy --locked --manifest-path /work/src/core/Cargo.toml --all-targets -- -D warnings
	$(CARGO) clippy --locked --manifest-path /work/src/native/Cargo.toml --all-targets -- -D warnings
	$(CARGO) fmt --manifest-path /work/src/server/Cargo.toml --all -- --check
	$(CARGO) clippy --locked --manifest-path /work/src/server/Cargo.toml --all-targets --all-features -- -D warnings

test: toolchain client-test
	$(CARGO) test --locked --manifest-path /work/src/core/Cargo.toml
	$(CARGO) test --locked --manifest-path /work/src/native/Cargo.toml
	$(CARGO) test --locked --manifest-path /work/src/third_party/mailparse/Cargo.toml
	$(CARGO) test --locked --manifest-path /work/src/server/Cargo.toml --all-features

# The client release workflow runs these two targets.
client-lint: toolchain
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --locked --all-targets -- -D warnings

client-test: toolchain
	$(CARGO) test --locked --lib --bin ahrara --test local --test web --test update_cli --test crypto_cli --test mcp_stdio
	$(DOCKER) -w /work python:3.13-slim python3 -B -m unittest discover -s tests -p 'test_release_manifest.py'
	$(DOCKER) -w /work -e AHRARA_TEST_BINARY=/work/.dev/target/debug/ahrara node:24-bookworm node --test tests/plugin.test.mjs

.PHONY: update-test
update-test: toolchain
	$(CARGO) test --locked --lib update::tests
	$(CARGO) test --locked --test update_cli
	$(DOCKER) -w /work python:3.13-slim python3 -B -m unittest discover -s tests -p 'test_release_manifest.py'

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
	$(CARGO) test --locked --test e2e email_routing_end_to_end -- --ignored --nocapture

toolchain:
	@mkdir -p .dev/cargo .dev/target
	if ! docker image inspect ahrara-builder:$(DOCKER_ARCH) >/dev/null 2>&1; then
		docker build --platform linux/$(DOCKER_ARCH) --target native -f src/cli/Dockerfile.builder -t ahrara-builder:$(DOCKER_ARCH) .
	fi

cargo: toolchain
	@$(CARGO) $(ARGS)

.PHONY: sdk-native sdk-rust-e2e
sdk-native: toolchain
	$(CARGO) build --locked --manifest-path /work/src/native/Cargo.toml
	native=$${AHRARA_NATIVE_LIBRARY:-$$SDK_NATIVE_LIBRARY}
	if [[ ! -f "$$native" || "$$native" != /* ]]; then
	  echo 'Build the native core first, or set AHRARA_NATIVE_LIBRARY to an absolute library path.' >&2
	  exit 1
	fi

sdk-rust-e2e: toolchain
	$(CARGO) test --locked --manifest-path /work/src/core/Cargo.toml --test embedded -- --ignored


# SDK dependencies, formatting, packaging and tests always run in containers.
SDK_NATIVE_LIBRARY ?= $(CURDIR)/.dev/target/debug/libahrara_native.so
SDK_OS ?= linux
SDK_ARCH ?= $(DOCKER_ARCH)
export SDK_NATIVE_LIBRARY AHRARA_WSS_PORT SDK_OS SDK_ARCH
export SDK_DOCKER_ARCH = $(DOCKER_ARCH)
# Development relay defaults; the environment overrides each one. Test containers mount the repository at /repo.
SDK_TESTS = sdk-dotnet-test sdk-java-test sdk-javascript-test sdk-python-test sdk-go-test sdk-k6-test
$(SDK_TESTS): export AHRARA_TEST_SERVER_URL ?= wss://localhost:8443
$(SDK_TESTS): export AHRARA_TEST_SMTP_ADDRESS ?= localhost:2525
$(SDK_TESTS): export AHRARA_TEST_TLS_CA ?= /repo/.dev/certs/ca.pem
$(SDK_TESTS): export AHRARA_TEST_SERVER_PUBLIC_KEY ?= $(shell cat .dev/certs/transport-public.txt 2>/dev/null)
GO_DOCKER = docker run --rm --network host -i -v "$(CURDIR):/repo" -w /repo --user "$(shell id -u):$(shell id -g)" \
	-e HOME=/tmp -e GOPATH=/repo/.dev/go -e GOCACHE=/repo/.dev/go-build -e GOFLAGS=-modcacherw \
	-e AHRARA_TEST_SERVER_URL -e AHRARA_TEST_SMTP_ADDRESS -e AHRARA_TEST_TLS_CA -e AHRARA_TEST_SERVER_PUBLIC_KEY \
	golang:1.27.1-bookworm sh -s

.PHONY: sdk-test sdk-dotnet-test sdk-java-test sdk-javascript-test sdk-python-test sdk-go-package sdk-go-test sdk-k6-test

sdk-dotnet-test: sdk-native
	set -euo pipefail
	repo="$$PWD"
	native=$${AHRARA_NATIVE_LIBRARY:-$$SDK_NATIVE_LIBRARY}

	docker run --rm --network host --user "$$(id -u):$$(id -g)" \
	  --volume "$$repo:/repo" --volume "$$native:/ahrara-native/libahrara_native.so:ro" --workdir /repo \
	  --env DOTNET_CLI_HOME=/repo/.dev/dotnet-home --env NUGET_PACKAGES=/repo/.dev/nuget \
	  --env AHRARA_TEST_SERVER_URL --env AHRARA_TEST_SMTP_ADDRESS \
	  --env AHRARA_TEST_TLS_CA --env AHRARA_TEST_SERVER_PUBLIC_KEY \
	  mcr.microsoft.com/dotnet/sdk:10.0 sh -ec '
	    mkdir -p src/sdks/dotnet/runtimes/linux-x64/native
	    cp /ahrara-native/libahrara_native.so src/sdks/dotnet/runtimes/linux-x64/native/
	    dotnet pack src/sdks/dotnet/src/Ahrara/Ahrara.csproj -p:PackageOutputPath=/repo/dist/sdks/dotnet --nologo
	    # Force a fresh restore of this locally built package, including its native asset.
	    rm -rf /repo/.dev/nuget/ahrara/0.1.0
	    dotnet test src/sdks/dotnet/tests/Ahrara.FrameworkTests/Ahrara.FrameworkTests.csproj \
	      -p:UsePackagedAhrara=true -p:RestoreAdditionalProjectSources=/repo/dist/sdks/dotnet \
	      --nologo --logger "trx;LogFileName=dotnet.trx"
	  '

sdk-java-test: sdk-native
	set -euo pipefail
	repo="$$PWD"
	native=$${AHRARA_NATIVE_LIBRARY:-$$SDK_NATIVE_LIBRARY}

	docker run --rm --network host --user "$$(id -u):$$(id -g)" \
	  --volume "$$repo:/repo" --volume "$$native:/ahrara-native/libahrara_native.so:ro" --workdir /repo \
	  --env MAVEN_CONFIG=/repo/.dev/maven \
	  --env AHRARA_TEST_SERVER_URL --env AHRARA_TEST_SMTP_ADDRESS \
	  --env AHRARA_TEST_TLS_CA --env AHRARA_TEST_SERVER_PUBLIC_KEY \
	  maven:3.9.11-eclipse-temurin-21 sh -ec '
	    mkdir -p src/sdks/java/src/main/resources/native/linux-x86_64
	    cp /ahrara-native/libahrara_native.so src/sdks/java/src/main/resources/native/linux-x86_64/
	    mvn -B -q -Dmaven.repo.local=/repo/.dev/maven/repository -f src/sdks/java/pom.xml \
	      package dependency:build-classpath -Dmdep.outputFile=target/test-classpath.txt
	    java -cp "src/sdks/java/target/ahrara-0.1.0.jar:src/sdks/java/target/test-classes:$$(cat src/sdks/java/target/test-classpath.txt)" \
	      dev.ahrara.PackagingSmoke
	  '

sdk-javascript-test: sdk-native
	set -euo pipefail
	project_root="$$PWD"
	native_library="$${AHRARA_NATIVE_LIBRARY:-$$SDK_NATIVE_LIBRARY}"
	docker run --rm --network host --shm-size=1g --user "$$(id -u):$$(id -g)" -e HOME=/tmp \
	  -v "$$project_root:/repo" -v "$$native_library:/native/libahrara_native.so:ro" \
	  -w /repo/src/sdks/javascript \
	  -e AHRARA_NATIVE_LIBRARY=/native/libahrara_native.so \
	  -e CYPRESS_CACHE_FOLDER=/repo/.dev/cypress \
	  -e AHRARA_TEST_SERVER_URL -e AHRARA_TEST_SERVER_PUBLIC_KEY \
	  -e AHRARA_TEST_SMTP_ADDRESS -e AHRARA_TEST_TLS_CA \
	  mcr.microsoft.com/playwright:v1.58.2-noble \
	  bash -ec '
	    npm ci
	    npm run format:check
	    npm run test:types
	    npm test
	    npm run test:playwright
	    xvfb-run --auto-servernum --server-num=199 npm run test:cypress
	    mkdir -p /repo/dist/javascript
	    install -D /native/libahrara_native.so "native/$$(node -p process.platform)-$$(node -p process.arch)/libahrara_native.so"
	    npm pack --pack-destination /repo/dist/javascript
	    consumer=$$(mktemp -d)
	    npm install --prefix "$$consumer" --silent /repo/dist/javascript/ahrara-sdk-0.1.0.tgz
	    cp test/package-smoke.mjs "$$consumer/"
	    unset AHRARA_NATIVE_LIBRARY
	    node "$$consumer/package-smoke.mjs" /repo/src/sdks/javascript/test/support.mjs
	  '

sdk-python-test: sdk-native
	set -euo pipefail
	project_root="$$PWD"
	native_library="$${AHRARA_NATIVE_LIBRARY:-$$SDK_NATIVE_LIBRARY}"
	docker run --rm --network host --user "$$(id -u):$$(id -g)" -e HOME=/tmp \
	  -v "$$project_root:/repo" -v "$$native_library:/native/libahrara_native.so:ro" \
	  -w /repo/src/sdks/python -e AHRARA_NATIVE_LIBRARY=/native/libahrara_native.so \
	  -e AHRARA_TEST_SERVER_URL -e AHRARA_TEST_SERVER_PUBLIC_KEY \
	  -e AHRARA_TEST_SMTP_ADDRESS -e AHRARA_TEST_TLS_CA \
	  python:3.13-slim bash -ec '
	    python -m venv /tmp/venv
	    . /tmp/venv/bin/activate
	    pip install --quiet pytest robotframework build black mypy
	    black --check ahrara tests
	    mypy --strict --explicit-package-bases ahrara
	    python -m pytest tests -q --tb=short
	    python -m robot --outputdir /repo/.dev/robot tests/inbox.robot
	    install -D /native/libahrara_native.so ahrara/native/libahrara_native.so
	    python -m build --wheel --outdir /repo/dist/python
	    pip install --quiet --force-reinstall /repo/dist/python/*.whl
	    cp tests/package_smoke.py /tmp/ahrara-package-smoke.py
	    cd /tmp
	    unset AHRARA_NATIVE_LIBRARY
	    python ahrara-package-smoke.py /repo/src/sdks/python/tests
	  '

sdk-go-package: sdk-native
	docker run --rm -i --user "$$(id -u):$$(id -g)" -v "$$PWD:/work" -v "$$SDK_NATIVE_LIBRARY:/ahrara-native/library:ro" -w /work ahrara-builder:$$SDK_DOCKER_ARCH sh -s -- /ahrara-native/library "$${SDK_OS:-linux}" "$${SDK_ARCH:-amd64}" <<'AHRARA_PACKAGE'
	set -eu

	# Run inside the Docker build image. No Rust compiler is needed by SDK consumers.
	case "$$2" in
	  linux) library=libahrara_native.so ;;
	  darwin) library=libahrara_native.dylib ;;
	  windows) library=ahrara_native.dll ;;
	  *) printf 'Unsupported native platform\n' >&2; exit 2 ;;
	esac
	case "$$3" in amd64|arm64) ;; *) printf 'Unsupported native architecture\n' >&2; exit 2 ;; esac
	destination="/work/src/sdks/go/native/$$2-$$3"
	mkdir -p "$$destination"
	staged=$$(mktemp "$$destination/.native.XXXXXX")
	trap 'rm -f "$$staged"' EXIT INT TERM
	cp "$$1" "$$staged"
	mv -f "$$staged" "$$destination/$$library"
	AHRARA_PACKAGE

sdk-go-test: sdk-go-package
	$(GO_DOCKER) <<'AHRARA_GO'
	set -eu

	# The integration suite deliberately exercises the embedded asset, without a
	# loader override.
	unset AHRARA_NATIVE_LIBRARY
	cd src/sdks/go
	go vet ./...
	go test -race -count=1 -v ./...
	AHRARA_GO

sdk-k6-test: sdk-go-package
	$(GO_DOCKER) <<'AHRARA_GO'
	set -eu

	# Run inside the Go Docker development image from the repository root.
	repo=$$(pwd)
	unset AHRARA_NATIVE_LIBRARY

	cd src/sdks/k6
	go build -buildvcs=false -o "$$repo/.dev/ahrara-k6" ./cmd/k6
	go build -buildvcs=false -o "$$repo/.dev/ahrara-k6-fixture" ./tests/fixture
	"$$repo/.dev/ahrara-k6-fixture" &
	fixture_pid=$$!
	trap 'kill "$$fixture_pid" 2>/dev/null || true' EXIT INT TERM
	for attempt in 1 2 3 4 5; do
	  if curl --fail --silent "$${AHRARA_TEST_FIXTURE_URL:-http://127.0.0.1:18787}/health" >/dev/null; then break; fi
	  sleep 1
	done
	"$$repo/.dev/ahrara-k6" run --no-usage-report tests/integration.js
	AHRARA_GO

sdk-test: dev-certs sdk-native
	set -euo pipefail
	repo="$$PWD"
	cd "$$repo"
	case "$$(uname -m)" in x86_64) docker_arch=amd64 ;; *) echo 'The complete SDK/browser test matrix currently requires a Linux x86_64 host.' >&2; exit 1 ;; esac

	# Make invokes the pinned builder container, including certificate generation.

	relay=
	cleanup() { if [[ -n "$$relay" ]]; then docker stop "$$relay" >/dev/null; fi; }
	trap cleanup EXIT
	relay=$$(docker run -d --rm --read-only --cap-drop ALL --security-opt no-new-privileges \
	  --user "$$(id -u):$$(id -g)" \
	  -p 127.0.0.1::8443 -p 127.0.0.1::2525 \
	  -v "$$repo/.dev/certs:/certs:ro" -v "$$repo/dist/ahrara-server:/ahrara-server:ro" \
	  --entrypoint /ahrara-server "ahrara-builder:$$docker_arch")
	ready=false
	for attempt in {1..30}; do
	  if docker exec "$$relay" /ahrara-server healthcheck >/dev/null 2>&1; then ready=true; break; fi
	  sleep 1
	done
	if [[ "$$ready" != true ]]; then echo 'SDK test relay did not become healthy' >&2; exit 1; fi
	wss=$$(docker port "$$relay" 8443/tcp)
	smtp=$$(docker port "$$relay" 2525/tcp)
	export AHRARA_TEST_SERVER_URL="wss://localhost:$${wss##*:}"
	export AHRARA_TEST_SMTP_ADDRESS="localhost:$${smtp##*:}"

	make sdk-rust-e2e
	make sdk-dotnet-test
	make sdk-java-test
	make sdk-javascript-test
	make sdk-python-test
	make sdk-go-test sdk-k6-test
