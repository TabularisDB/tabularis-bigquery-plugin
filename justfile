set shell := ["bash", "-cu"]
set windows-shell := ["powershell.exe", "-NoLogo", "-NoProfile", "-Command"]

build:
    cargo build

release:
    cargo build --release

test:
    cargo test --all-targets

lint:
    cargo clippy --all-targets -- -D warnings

fmt:
    cargo fmt --all

check-ui:
    node --check ui/connection-fields.js

[linux]
dev-install: build
    mkdir -p ~/.local/share/tabularis/plugins/bigquery/ui
    cp target/debug/bigquery-plugin ~/.local/share/tabularis/plugins/bigquery/
    cp .tabularium ~/.local/share/tabularis/plugins/bigquery/
    cp ui/connection-fields.js ~/.local/share/tabularis/plugins/bigquery/ui/

[macos]
dev-install: build
    mkdir -p "$HOME/Library/Application Support/com.debba.tabularis/plugins/bigquery/ui"
    cp target/debug/bigquery-plugin "$HOME/Library/Application Support/com.debba.tabularis/plugins/bigquery/"
    cp .tabularium "$HOME/Library/Application Support/com.debba.tabularis/plugins/bigquery/"
    cp ui/connection-fields.js "$HOME/Library/Application Support/com.debba.tabularis/plugins/bigquery/ui/"

