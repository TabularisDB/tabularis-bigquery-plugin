<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
</div>

# tabularis-bigquery-plugin

A native Google BigQuery driver for [Tabularis](https://github.com/TabularisDB/tabularis). The plugin is a standalone Rust executable that uses the BigQuery REST API v2 and communicates with Tabularis through newline-delimited JSON-RPC 2.0 over standard input and output.

## Features

- Application Default Credentials, service account JSON, refreshable OAuth authorized user credentials and direct OAuth access token authentication
- Dataset, table, external table, snapshot, clone, view and materialized view browsing
- Nested `STRUCT`, repeated `ARRAY`, `JSON`, `BYTES`, temporal, numeric and geography value conversion
- GoogleSQL query execution with result pagination, job polling, retry handling and DML affected-row counts
- Visual EXPLAIN cost summaries backed by BigQuery dry runs
- Per-connection location, timeout and maximum bytes billed controls
- Table columns, descriptions, primary keys and foreign keys from BigQuery metadata
- Routine discovery and routine definition inspection
- Inline insert, update and delete operations with type-aware GoogleSQL literals
- Table, column, view and unenforced key DDL generation
- BLOB preview and export for `BYTES` columns
- Batch metadata methods for ER diagrams and AI schema context
- Linux, macOS and Windows release packaging

## Connection setup

On a compatible Tabularis host, the BigQuery plugin hides unused common fields and maps the connection form as follows:

| Tabularis field | BigQuery meaning |
| --- | --- |
| GCP Project ID | Project used for jobs and billing |
| Credential | Service account JSON/path, authorized user JSON/path, or access token |

The BigQuery section in the connection form adds:

- **Authentication**: ADC, service account JSON, OAuth authorized user JSON, or OAuth access token
- **Location**: optional job location such as `US`, `EU`, or `europe-west1`
- **Query timeout**: 30 to 3600 seconds, default 300
- **Maximum bytes billed**: optional per-query cost guard

The Credential field is plugin-owned. Tabularis stores its value in the OS keychain and never writes it to `connections.json`. Existing connections that used the Password field remain supported and do not require migration.

Older Tabularis hosts ignore the field presentation overrides. On those hosts, continue to use Password for credentials and enable keychain storage.

### Application Default Credentials

Leave Credential empty. The plugin tries the standard Google credential chain:

1. `GOOGLE_APPLICATION_CREDENTIALS`
2. Credentials created by `gcloud auth application-default login`
3. A service account attached to the Google Cloud runtime
4. The active `gcloud` CLI account

### Service account

Select **Service account JSON** and put either the credential file path or the complete JSON document in Credential.

### OAuth access token

Select **OAuth access token** and put a current bearer token in Credential. Direct access tokens expire and the plugin cannot refresh them. ADC is recommended for interactive user credentials.

### OAuth authorized user

Select **OAuth authorized user JSON** and put either an `authorized_user` credential file path or the complete JSON document in Credential. The document must contain `client_id`, `client_secret` and `refresh_token`. The plugin exchanges the refresh token only with Google's fixed OAuth token endpoint and caches short-lived access tokens.

## Required Google Cloud permissions

The exact IAM role depends on the enabled operations. Read-only use generally needs BigQuery Metadata Viewer and BigQuery Job User permissions. Editing and DDL need the corresponding BigQuery Data Editor or Data Owner permissions. Grant the least privilege required by your workflow.

## Installation

Download the archive for your platform from GitHub Releases, extract it, and copy the complete `bigquery` folder into the Tabularis plugin directory. The folder must contain:

```text
bigquery/
├── .tabularium
├── bigquery-plugin
└── ui/
    └── dist/
        └── connection-fields.js
```

Use `bigquery-plugin.exe` on Windows.

| OS | Plugin directory |
| --- | --- |
| Linux | `~/.local/share/tabularis/plugins/bigquery/` |
| macOS | `~/Library/Application Support/com.debba.tabularis/plugins/bigquery/` |
| Windows | `%APPDATA%\debba\tabularis\data\plugins\bigquery\` |

## Build and test

Rust stable, Node.js 20 and pnpm 9 are required. The UI extension is authored in React and TypeScript under `ui/src`; Vite produces the IIFE bundle loaded by Tabularis.

```bash
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui typecheck
pnpm --dir ui build
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Validate the release manifest with Tabularium:

```bash
npx --yes @tabularium/cli validate .tabularium \
  --registry https://registry.tabularis.dev \
  --kind driver
```

## JSON-RPC smoke test

```bash
echo '{"jsonrpc":"2.0","method":"initialize","id":1}' \
  | cargo run --quiet
```

The plugin writes protocol responses only to stdout. Diagnostics are written to stderr.

## BigQuery-specific behavior

- Datasets are exposed as Tabularis schemas.
- The selected project is used as the BigQuery job project and billing project.
- BigQuery primary and foreign keys are informational and emitted as `NOT ENFORCED` in generated DDL.
- BigQuery materialized views refresh automatically, so manual refresh returns an unsupported-operation error.
- General-purpose indexes are not available in BigQuery. Search indexes and vector indexes require specialized statements and are not exposed through Tabularis' generic index UI.
- Visual EXPLAIN uses a BigQuery dry run. It reports estimated bytes processed without executing the query, rather than exposing an operator tree.

## License

Apache-2.0.
