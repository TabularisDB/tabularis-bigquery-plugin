<div align="center">
  <img src="https://raw.githubusercontent.com/TabularisDB/tabularis/main/public/logo-sm.png" width="120" height="120" alt="Tabularis logo" />
</div>

# tabularis-bigquery-plugin

A native Google BigQuery driver for [Tabularis](https://github.com/TabularisDB/tabularis). The plugin is a standalone Rust executable that uses the BigQuery REST API v2 and communicates with Tabularis through newline-delimited JSON-RPC 2.0 over standard input and output.

## Features

- Application Default Credentials, service account JSON, refreshable OAuth authorized user credentials and direct OAuth access token authentication
- Multi-project browsing: every project the credential can see, plus extra projects such as `bigquery-public-data`
- Dataset, table, external table, snapshot, clone, view and materialized view browsing with descriptions
- Nested `STRUCT`, repeated `ARRAY`, `JSON`, `BYTES`, temporal, numeric and geography value conversion
- GoogleSQL query execution with result pagination, job polling, retry handling and DML affected-row counts
- Visual EXPLAIN cost summaries backed by BigQuery dry runs
- Per-connection location, timeout and maximum bytes billed controls
- Table columns, descriptions, parameterized types, primary keys and foreign keys from BigQuery metadata
- Table, view and materialized view DDL from `INFORMATION_SCHEMA`, with partitioning, clustering and options
- Routine management for SQL, JavaScript and Python functions, table functions, aggregate functions and procedures: browse, inspect parameters, run, create, edit and drop
- Manual materialized view refresh
- Inline insert, update and delete with typed query parameters, including tables without a primary key
- Table, column, view and unenforced key DDL generation that only emits changes BigQuery accepts
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

- **Authentication**: ADC, service account JSON, OAuth authorized user JSON, or OAuth access token
- **Location**: optional job location such as `US`, `EU`, or `europe-west1`
- **Query timeout**: 30 to 3600 seconds, default 300
- **Maximum bytes billed**: optional per-query cost guard
- **Billing project** (`billing_project`): optional project that runs and pays for every job. Set it when browsing several projects, so jobs never run in a project you can only read, such as `bigquery-public-data`
- **Additional projects** (`additional_projects`): optional comma-separated projects added to the project list, for public or shared projects the credential cannot list

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

Live metadata, DDL and row-editing tests run against the BigQuery emulator when `BIGQUERY_EMULATOR_HOST` is set and are skipped otherwise:

```bash
docker run --rm -p 9050:9050 ghcr.io/goccy/bigquery-emulator:latest \
  --project=test-project --dataset=sales
BIGQUERY_EMULATOR_HOST=http://localhost:9050 cargo test --test emulator
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

- Projects are exposed as Tabularis databases and datasets as schemas. Select several projects in the connection dialog to browse them side by side.
- Jobs always run in the billing project: the `billing_project` setting when present, otherwise the GCP Project ID. Metadata, DDL and row edits target the project being browsed, and generated SQL is fully qualified as `` `project`.`dataset`.`object` `` with every part quoted separately, so project IDs containing dashes work.
- Metadata comes from the free REST API (`tables.get`, `routines.get`). Resources are fetched once per object, with at most eight requests in flight for ER diagrams, schema snapshots and AI context, and cached for five seconds; any DDL sent through the plugin clears the cache. Table descriptions are loaded for datasets with up to 100 tables or views.
- Table, view and materialized view definitions come from `INFORMATION_SCHEMA.TABLES.ddl`, a small billed query run in the dataset location, so options, partitioning and clustering are preserved. When that query is not available the DDL is rebuilt from table metadata.
- Saving a view keeps its description, friendly name and labels. Legacy SQL views can be read but not saved from the view editor.
- Routines are listed as functions and procedures. Table functions, aggregate functions, JavaScript and Python UDFs and remote functions are rebuilt as complete `CREATE OR REPLACE` scripts for editing. Running a function previews `SELECT fn(...)` or `SELECT * FROM table_fn(...)`; procedures use `CALL`, with `OUT` and `INOUT` arguments returned through script variables.
- Materialized views refresh automatically. **Refresh** runs `CALL BQ.REFRESH_MATERIALIZED_VIEW(...)` for an immediate refresh.
- BigQuery primary and foreign keys are informational and emitted as `NOT ENFORCED`. Foreign keys support no `ON DELETE` / `ON UPDATE` actions.
- Column changes follow BigQuery limits: new columns cannot be `NOT NULL`, existing columns can drop `NOT NULL` but never gain it, and `SET DATA TYPE` only widens types (`INT64` to `NUMERIC`, `BIGNUMERIC` or `FLOAT64`; `NUMERIC` to `BIGNUMERIC` or `FLOAT64`; larger `STRING`, `BYTES` and decimal parameters). Other changes are rejected with an explanation instead of failing at run time.
- Snapshots and external tables are read-only for row editing and column changes.
- Row edits use typed query parameters. On tables without a primary key, rows are matched on every comparable column (floating point, JSON, geography, arrays, structs, ranges and bytes are skipped), and the change runs in a transaction that rolls back if more than one row would change.
- Rows still in the streaming buffer cannot be updated or deleted; the plugin reports this with a clear message.
- General-purpose indexes are not available in BigQuery. Search and vector indexes use `CREATE SEARCH INDEX` / `CREATE VECTOR INDEX` in a query tab; the index editor only drops the informational primary key.
- Visual EXPLAIN uses a BigQuery dry run. It reports estimated bytes processed without executing the query, rather than exposing an operator tree.

## License

Apache-2.0.
