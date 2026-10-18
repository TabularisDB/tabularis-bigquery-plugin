<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
</div>

# tabularis-bigquery-plugin

A native Google BigQuery driver for [Tabularis](https://github.com/TabularisDB/tabularis). The plugin is a standalone Rust executable that uses the BigQuery REST API v2 and communicates with Tabularis through newline-delimited JSON-RPC 2.0 over standard input and output.

## Features

- Application Default Credentials (including `impersonated_service_account` ADC files), service account JSON, refreshable OAuth authorized user credentials and direct OAuth access token authentication
- Optional service account impersonation for every authentication method
- Optional API endpoint override for Private Service Connect and local emulators
- Dataset, table, external table, snapshot, clone, view and materialized view browsing
- Nested `STRUCT`, repeated `ARRAY`, `JSON`, `BYTES`, temporal, numeric and geography value conversion
- GoogleSQL query execution with idempotent, labelled jobs, server-side job timeouts and cancellation
- Exact result pagination that reuses the finished job for later pages instead of running the SQL again
- Free table browsing through `tabledata.list` for plain `SELECT * FROM table` queries
- Editor sessions: temporary tables, variables and multi-statement transactions survive between runs
- Multi-statement scripts return the last result set and the total DML affected-row count
- Visual EXPLAIN with dry-run cost estimates and executed stage plans (`EXPLAIN ANALYZE`)
- Clear error messages with hints for common BigQuery failures
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

- **Authentication**: ADC, service account JSON, OAuth authorized user JSON, OAuth access token, or no authentication (local emulator only)
- **Location**: optional job location such as `US`, `EU`, or `europe-west1`
- **Query timeout**: 30 to 3600 seconds, default 110
- **Maximum bytes billed**: optional per-query cost guard
- **Advanced options**: API endpoint and service account to impersonate

The query timeout is also sent to BigQuery as the job timeout, and the plugin cancels a job that exceeds it. Tabularis waits 120 seconds for a plugin call by default, so the default of 110 seconds lets the plugin report and cancel a slow query itself. To allow longer queries, raise both the connection's query timeout and the Tabularis plugin call timeout (`pluginCallTimeoutSeconds`, or `callTimeoutSeconds` for the `bigquery` plugin). When Tabularis gives up on a call it sends a cancel notification and the plugin cancels the running job.

The Credential field is plugin-owned. Tabularis stores its value in the OS keychain and never writes it to `connections.json`. Existing connections that used the Password field remain supported and do not require migration.

Older Tabularis hosts ignore the field presentation overrides. On those hosts, continue to use Password for credentials and enable keychain storage.

### Application Default Credentials

Leave Credential empty. The plugin tries the standard Google credential chain:

1. `GOOGLE_APPLICATION_CREDENTIALS`
2. Credentials created by `gcloud auth application-default login`
3. A service account attached to the Google Cloud runtime
4. The active `gcloud` CLI account

If the ADC file (`GOOGLE_APPLICATION_CREDENTIALS` or the gcloud default file) is an `impersonated_service_account` credential, such as one created by `gcloud auth application-default login --impersonate-service-account`, the plugin exchanges its source credential for a token of the target service account through the IAM Credentials API.

### Service account

Select **Service account JSON** and put either the credential file path or the complete JSON document in Credential. The key's `token_uri` must be a `googleapis.com` HTTPS endpoint; keys pointing elsewhere are rejected. A leading byte order mark is tolerated.

### Service account impersonation

Enter a service account email under **Advanced options > Impersonate service account** to run every request as that account. The configured credential must hold the Service Account Token Creator role on it.

### API endpoint and emulators

**Advanced options > API endpoint** replaces `https://bigquery.googleapis.com`, for example with a Private Service Connect endpoint. The REST path `/bigquery/v2/` is appended when missing. HTTPS is required except for `localhost`, `127.0.0.1` and `[::1]`. The **No authentication** method is accepted only together with such a local endpoint, for example `http://localhost:9050` for a BigQuery emulator.

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
├── explain/
│   └── dist/
│       └── index.iife.js
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

Rust stable, Node.js 20 and pnpm 9 are required. The UI extension is authored in React and TypeScript under `ui/src`; Vite produces the IIFE bundle loaded by Tabularis. The Visual EXPLAIN parser lives under `explain/` and is built with tsup.

```bash
pnpm --dir ui install --frozen-lockfile
pnpm --dir ui typecheck
pnpm --dir ui build
pnpm --dir explain install --frozen-lockfile
pnpm --dir explain typecheck
pnpm --dir explain test
cargo build
cargo test --all-targets
cargo clippy --all-targets -- -D warnings
cargo fmt --all -- --check
```

Integration tests run against the BigQuery emulator when `BIGQUERY_EMULATOR_HOST` is set and are skipped otherwise:

```bash
docker run -d -p 9050:9050 ghcr.io/goccy/bigquery-emulator:latest --project=test --dataset=ds
BIGQUERY_EMULATOR_HOST=localhost:9050 cargo test --test emulator
```

The emulator does not implement sessions, dry runs, DML row counts, script child jobs or `tabledata.list` paging; the plugin falls back gracefully and the mocked tests in `tests/runtime_mock.rs` cover those paths.

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

The plugin writes protocol responses only to stdout. Diagnostics are written to stderr. Requests are handled concurrently, so responses can arrive in a different order than requests; each carries its request `id`. Notifications, such as `cancel`, are never answered.

## BigQuery-specific behavior

- Datasets are exposed as Tabularis schemas.
- The selected project is used as the BigQuery job project and billing project.
- BigQuery primary and foreign keys are informational and emitted as `NOT ENFORCED` in generated DDL.
- BigQuery materialized views refresh automatically, so manual refresh returns an unsupported-operation error.
- General-purpose indexes are not available in BigQuery. Search indexes and vector indexes require specialized statements and are not exposed through Tabularis' generic index UI.
- Jobs carry a `requestId`, so retried requests never start a second job, and the label `client: tabularis` for cost attribution. Requests are retried on HTTP 429 and 5xx and on 403 `rateLimitExceeded` or `backendError`, with exponential backoff and jitter. A rejected token is refreshed once.
- Pages after the first are read from the finished job with `getQueryResults` for 10 minutes; the SQL is not executed again. The first page always runs the statement.
- A query of the exact form `SELECT * FROM table [LIMIT n [OFFSET m]]` on a native table, which is what a Tabularis table tab sends, is served by `tabledata.list` without a query job and without scanning cost. Views, external tables, filters, ordering and any other SQL run as normal jobs.
- TIMESTAMP values are returned with exact microseconds.
- When Tabularis pins an editor tab to a session, the plugin creates a BigQuery session for it and runs every statement of that tab inside it. `BEGIN`, `COMMIT` and `ROLLBACK` are tracked for the transaction indicator. Closing the tab aborts the session. A batch without a session that contains `DECLARE`, `SET`, temporary tables or transaction statements runs in a short-lived session. BigQuery has no savepoints; savepoint statements are answered with an error without being sent.
- Visual EXPLAIN runs a dry run and shows bytes processed and an estimated on-demand cost (US$6.25 per TiB list price, clearly labelled as an estimate) plus the referenced tables. EXPLAIN ANALYZE executes the statement, including DML, and shows each query stage with slot time, wall time, records read and written, shuffle bytes and steps. The plan is parsed by the bundled `explain/dist/index.iife.js` parser, which requires Tabularis 0.23.0 or newer.

## License

Apache-2.0.
