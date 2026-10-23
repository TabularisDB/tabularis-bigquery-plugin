<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/packages/web-ui/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-bigquery-plugin/main/assets/plus.svg" width="40" height="40" alt="plus" />
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis-bigquery-plugin/main/bigquery-icon.svg" width="120" height="120" alt="Google BigQuery logo" />
</div>

# tabularis-bigquery-plugin

<p align="center">

![Release](https://img.shields.io/github/v/release/TabularisDB/tabularis-bigquery-plugin?include_prereleases&style=flat)
![Downloads](https://img.shields.io/github/downloads/TabularisDB/tabularis-bigquery-plugin/total.svg?style=flat)
![CI](https://github.com/TabularisDB/tabularis-bigquery-plugin/workflows/CI/badge.svg)

</p>

A native Google BigQuery driver for [Tabularis](https://github.com/TabularisDB/tabularis). The plugin is a standalone Rust executable that uses the BigQuery REST API v2 and communicates with Tabularis through newline-delimited JSON-RPC 2.0 over standard input and output.

Requires Tabularis 0.29.0 or later.

## Features

- Application Default Credentials (including `impersonated_service_account` ADC files), service account JSON, refreshable OAuth authorized user credentials and direct OAuth access token authentication
- Optional service account impersonation for every authentication method
- Optional API endpoint override for Private Service Connect and local emulators
- Multi-project browsing: every project the credential can see, plus extra projects such as `bigquery-public-data`, with an optional separate billing project for jobs
- Dataset, table, external table, snapshot, clone, view and materialized view browsing with descriptions
- Nested `STRUCT`, repeated `ARRAY`, `JSON`, `BYTES`, temporal, numeric and geography value conversion
- GoogleSQL editor support declared through `sql_syntax`: script splitting with `BEGIN … END` blocks, triple-quoted and raw strings, `#` comments, BigQuery formatting and autocomplete keywords
- GoogleSQL query execution with idempotent, labelled jobs, server-side job timeouts and cancellation
- Exact result pagination that reuses the finished job for later pages instead of running the SQL again
- Free table browsing through `tabledata.list` for plain `SELECT * FROM table` queries
- Editor sessions: temporary tables, variables and multi-statement transactions survive between runs
- Multi-statement scripts return the last result set and the total DML affected-row count
- Visual EXPLAIN with dry-run cost estimates and executed stage plans (`EXPLAIN ANALYZE`)
- Clear error messages with hints for common BigQuery failures
- Per-connection location, timeout and maximum bytes billed controls
- Table columns, descriptions, parameterized types, primary keys and foreign keys from BigQuery metadata
- Table, view and materialized view DDL from `INFORMATION_SCHEMA`, with partitioning, clustering and options
- Routine management for SQL, JavaScript and Python functions, table functions, aggregate functions and procedures: browse, inspect parameters, run, create, edit and drop
- Manual materialized view refresh
- Inline insert, update and delete with typed query parameters, including tables without a primary key
- Table, column, view and unenforced key DDL generation that only emits changes BigQuery accepts, with column descriptions
- Dropping tables, external tables, snapshots, views and materialized views from the explorer with the matching `DROP` statement
- BLOB preview and export for `BYTES` columns
- Batch metadata methods for ER diagrams and AI schema context, fetched with bounded concurrency
- Linux, macOS and Windows release packaging

## Connection setup

On a compatible Tabularis host, the BigQuery plugin hides unused common fields and maps the connection form as follows:

| Tabularis field | BigQuery meaning |
| --- | --- |
| GCP Project ID | Project that is browsed and, unless a billing project is set, runs jobs |
| Credential | Service account JSON/path, authorized user JSON/path, or access token |

The BigQuery section in the connection form adds:

- **Authentication**: ADC, service account JSON, OAuth authorized user JSON, OAuth access token, or no authentication (local emulator only)
- **Location**: optional job location such as `US`, `EU`, or `europe-west1`
- **Query timeout**: 30 to 3600 seconds, default 110
- **Maximum bytes billed**: optional per-query cost guard
- **Advanced options**:
  - **Billing project** (`billing_project`): optional project that runs and pays for every job. Set it when browsing several projects, so jobs never run in a project you can only read, such as `bigquery-public-data`
  - **Additional projects** (`additional_projects`): optional comma-separated projects added to the project list, for public or shared projects the credential cannot list
  - **API endpoint** and **Impersonate service account**, described below

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

The exact IAM role depends on the enabled operations. Grant the least privilege required by your workflow.

| Operation | Permissions | Typical role |
| --- | --- | --- |
| Run queries, DDL and DML | `bigquery.jobs.create` on the billing project | BigQuery Job User |
| Browse datasets, tables, views and routines | `bigquery.datasets.get`, `bigquery.tables.list`, `bigquery.tables.get`, `bigquery.routines.list`, `bigquery.routines.get` | BigQuery Metadata Viewer |
| Read table data and `INFORMATION_SCHEMA` DDL | `bigquery.tables.getData` | BigQuery Data Viewer |
| List projects for multi-project browsing | `resourcemanager.projects.get` on each listed project | Any role granting BigQuery access to the project |
| Edit rows, alter tables, create and drop views and routines | `bigquery.tables.update`, `bigquery.tables.updateData`, `bigquery.tables.create`, `bigquery.tables.delete`, `bigquery.routines.create`, `bigquery.routines.update`, `bigquery.routines.delete` | BigQuery Data Editor |
| Refresh materialized views | `bigquery.tables.updateData` on the view and read access to its base tables | BigQuery Data Editor |

When projects cannot be listed, the project list falls back to the configured, billing and additional projects.

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
| Linux | `~/.local/share/tabularis/plugins/drivers/bigquery/` |
| macOS | `~/Library/Application Support/tabularis/plugins/drivers/bigquery/` |
| Windows | `%APPDATA%\tabularis\plugins\drivers\bigquery\` |

## Build and test

Rust stable, Node.js 20 and pnpm 9 or later are required. Each package directory is its own pnpm workspace whose `pnpm-workspace.yaml` allows the esbuild install script, so frozen installs also work with pnpm 10 and later. The UI extension is authored in React and TypeScript under `ui/src`; Vite produces the IIFE bundle loaded by Tabularis. The Visual EXPLAIN parser lives under `explain/` and is built with tsup.

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

Integration tests run against the BigQuery emulator when `BIGQUERY_EMULATOR_HOST` is set and are skipped otherwise. `tests/emulator.rs` covers the query runtime and `tests/emulator_metadata.rs` covers metadata, DDL and row editing; both expect the project `test` and the dataset `ds`:

```bash
docker run -d -p 9050:9050 ghcr.io/goccy/bigquery-emulator:latest --project=test --dataset=ds
BIGQUERY_EMULATOR_HOST=localhost:9050 cargo test --all-targets
```

The emulator does not implement sessions, dry runs, DML row counts, script child jobs, `tabledata.list` paging, `@@row_count`, `INFORMATION_SCHEMA.TABLES.ddl`, informational keys or the routines API; the plugin falls back gracefully and the mocked tests in `tests/runtime_mock.rs` and the host-shape tests in `tests/conformance.rs` and `tests/conformance_metadata.rs` cover those paths.

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

- Projects are exposed as Tabularis databases and datasets as schemas. Select several projects in the connection dialog to browse them side by side.
- Jobs always run in the billing project: the `billing_project` setting when present, otherwise the GCP Project ID. Unqualified table names in the editor resolve in the project being browsed (`defaultDataset.projectId`), and metadata, DDL and row edits target that project too. Generated SQL is fully qualified as `` `project`.`dataset`.`object` `` with every part quoted separately, so project IDs containing dashes work.
- Metadata comes from the free REST API (`tables.get`, `routines.get`). Resources are fetched once per object, with at most eight requests in flight for ER diagrams, schema snapshots and AI context, and cached for five seconds; any DDL sent through the plugin, and any statement other than a plain query run from the editor, clears the cache. Table descriptions are loaded for datasets with up to 100 tables or views.
- Jobs carry a `requestId`, so retried requests never start a second job, and the label `client: tabularis` for cost attribution. Requests are retried on HTTP 429 and 5xx and on 403 `rateLimitExceeded` or `backendError`, with exponential backoff and jitter. A rejected token is refreshed once.
- Pages after the first are read from the finished job with `getQueryResults` for 10 minutes; the SQL is not executed again. The first page always runs the statement.
- A query of the exact form `SELECT * FROM table [LIMIT n [OFFSET m]]` on a native table, which is what a Tabularis table tab sends, is served by `tabledata.list` without a query job and without scanning cost. Views, external tables, filters, ordering and any other SQL run as normal jobs.
- TIMESTAMP values are returned with exact microseconds.
- When Tabularis pins an editor tab to a session, the plugin creates a BigQuery session for it and runs every statement of that tab inside it. `BEGIN`, `COMMIT` and `ROLLBACK` are tracked for the transaction indicator. Closing the tab aborts the session. A batch without a session that contains `DECLARE`, `SET`, temporary tables or transaction statements runs in a short-lived session. BigQuery has no savepoints; savepoint statements are answered with an error without being sent.
- Visual EXPLAIN runs a dry run and shows bytes processed and an estimated on-demand cost (US$6.25 per TiB list price, clearly labelled as an estimate) plus the referenced tables. EXPLAIN ANALYZE executes the statement, including DML, and shows each query stage with slot time, wall time, records read and written, shuffle bytes and steps. The plan is parsed by the bundled `explain/dist/index.iife.js` parser.
- Table, view and materialized view definitions come from `INFORMATION_SCHEMA.TABLES.ddl`, a small billed query run in the dataset location, so options, partitioning and clustering are preserved. When that query is not available the DDL is rebuilt from table metadata.
- Saving a view keeps its description, friendly name and labels. Legacy SQL views can be read but not saved from the view editor.
- Routines are listed as functions and procedures. Table functions, aggregate functions, JavaScript and Python UDFs and remote functions are rebuilt as complete `CREATE OR REPLACE` scripts for editing. Running a function previews `SELECT fn(...)` or `SELECT * FROM table_fn(...)`; procedures use `CALL`, with `OUT` and `INOUT` arguments returned through script variables.
- Materialized views refresh automatically. **Refresh** runs `CALL BQ.REFRESH_MATERIALIZED_VIEW(...)` for an immediate refresh.
- BigQuery primary and foreign keys are informational and emitted as `NOT ENFORCED`. Foreign keys support no `ON DELETE` / `ON UPDATE` actions.
- Column changes follow BigQuery limits: new columns cannot be `NOT NULL`, existing columns can drop `NOT NULL` but never gain it, and `SET DATA TYPE` only widens types (`INT64` to `NUMERIC`, `BIGNUMERIC` or `FLOAT64`; `NUMERIC` to `BIGNUMERIC` or `FLOAT64`; larger `STRING`, `BYTES` and decimal parameters). Other changes are rejected with an explanation instead of failing at run time.
- Snapshots and external tables are read-only for row editing and column changes.
- Column descriptions entered in the column editor become `OPTIONS(description = ...)` on new tables and columns, and `ALTER COLUMN ... SET OPTIONS(description = ...)` on existing columns; clearing the description sets it to `NULL`.
- Dropping an object from the explorer looks up its type and runs `DROP TABLE`, `DROP EXTERNAL TABLE`, `DROP SNAPSHOT TABLE`, `DROP VIEW` or `DROP MATERIALIZED VIEW` on the fully qualified name.
- Row edits use typed query parameters. On tables without a primary key, rows are matched on every comparable column (floating point, JSON, geography, arrays, structs, ranges and bytes are skipped), and the change runs in a transaction that rolls back if more than one row would change.
- Rows still in the streaming buffer cannot be updated or deleted; the plugin reports this with a clear message.
- General-purpose indexes are not available in BigQuery. Search and vector indexes use `CREATE SEARCH INDEX` / `CREATE VECTOR INDEX` in a query tab; the index editor only drops the informational primary key.
- Metadata queries, DDL and row edits run through the same job path as editor queries: `requestId`, retries, `jobTimeoutMs`, labels, error hints and cancellation apply to every statement the plugin sends.

## License

Apache-2.0.
