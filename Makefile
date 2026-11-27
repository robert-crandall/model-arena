.PHONY: help init setup build install-mcp mcp-token mcp-config run dev test check spec clean

.DELETE_ON_ERROR:

help: ## Show this help
	@grep -hE '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) \
		| awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-12s\033[0m %s\n", $$1, $$2}'

init: ## Initialize template (MODULE=github.com/you/app NAME="My App" [SLUG=my-app])
	@scripts/init.sh "$(MODULE)" "$(NAME)" "$(SLUG)"

setup: ## First run after cloning: frontend deps, .env, and an initial frontend build
	cd web && bun install --frozen-lockfile
	@test -f .env || { cp .env.example .env && echo "wrote .env from .env.example"; }
	cd web && bun run build

# Stage binaries via mktemp + mv so macOS doesn't reuse a cached vnode code signature on overwrite.
build: ## Build the Solid 2.0 SPA, then compile the release binary into bin/
	cd web && bun run build
	mkdir -p bin
	cargo build --locked --release --bin model-arena-rust
	tmp=$$(mktemp "bin/.model-arena-rust.XXXXXX") || exit 1; \
	trap 'rm -f "$$tmp"' EXIT; \
	cp target/release/model-arena-rust "$$tmp" && \
	chmod 0755 "$$tmp" && \
	mv -f "$$tmp" bin/model-arena-rust

install-mcp: ## Build and install model-arena-rust-mcp into ~/bin
	mkdir -p "$(HOME)/bin"
	cargo build --locked --release --bin model-arena-rust-mcp
	tmp=$$(mktemp "$(HOME)/bin/.model-arena-rust-mcp.XXXXXX") || exit 1; \
	trap 'rm -f "$$tmp"' EXIT; \
	cp target/release/model-arena-rust-mcp "$$tmp" && \
	chmod 0755 "$$tmp" && \
	mv -f "$$tmp" "$(HOME)/bin/model-arena-rust-mcp"
	@echo "installed $(HOME)/bin/model-arena-rust-mcp"

mcp-token: ## Sign in and mint an MCP token into ~/.config/model-arena-rust.json
	cargo run --locked --bin model-arena-rust-mcp -- token

mcp-config: ## Print the local stdio MCP client configuration
	@printf '{\n  "mcpServers": {\n    "model-arena-rust": { "command": "%s/bin/model-arena-rust-mcp", "args": ["serve"] }\n  }\n}\n' \
		"$(HOME)"

run: ## Run the release binary built by `make build`
	./bin/model-arena-rust

dev: ## Run Vite on :5173 with the Rust API server on :8080
	@scripts/dev.sh

web/build/index.html:
	cd web && bun run build

test: web/build/index.html ## Run Rust unit/integration tests and frontend Vitest suite
	cargo test --locked
	cd web && bun run test

check: web/build/index.html ## Run rustfmt check, clippy (-D warnings), and frontend tsc
	cargo fmt --all -- --check
	cargo clippy --all-targets --locked -- -D warnings
	cd web && bun run check

spec: ## Regenerate docs/openapi.json and web/src/lib/api/schema.d.ts from Rust routes
	cargo run --locked --bin openapi
	cd web && bun run gen:api

clean: ## Remove build artifacts
	rm -rf target bin .bin web/build
