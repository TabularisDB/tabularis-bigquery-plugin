set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

build:
    cargo build

release:
    cargo build --release

ui-install:
    pnpm --dir ui install

ui-build:
    pnpm --dir ui build

explain-build:
    pnpm --dir explain build

ui-typecheck:
    pnpm --dir ui typecheck

test:
    cargo test --all-targets

lint:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt --all

check-ui:
    pnpm --dir ui typecheck
    pnpm --dir ui build

[linux]
dev-install: build ui-build explain-build
    mkdir -p ~/.local/share/tabularis/plugins/drivers/bigquery/ui/dist ~/.local/share/tabularis/plugins/drivers/bigquery/explain/dist
    cp target/debug/bigquery-plugin ~/.local/share/tabularis/plugins/drivers/bigquery/
    cp .tabularium ~/.local/share/tabularis/plugins/drivers/bigquery/
    cp ui/dist/*.js ~/.local/share/tabularis/plugins/drivers/bigquery/ui/dist/
    cp explain/dist/index.iife.js ~/.local/share/tabularis/plugins/drivers/bigquery/explain/dist/

[macos]
dev-install: build ui-build explain-build
    mkdir -p "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/ui/dist" "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/explain/dist"
    cp target/debug/bigquery-plugin "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/"
    cp .tabularium "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/"
    cp ui/dist/*.js "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/ui/dist/"
    cp explain/dist/index.iife.js "$HOME/Library/Application Support/tabularis/plugins/drivers/bigquery/explain/dist/"
