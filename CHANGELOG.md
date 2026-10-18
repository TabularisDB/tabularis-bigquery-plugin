# Changelog

All notable changes to this project are documented in this file.

## 0.2.0

Requires Tabularis 0.29.0 or later (`min_runtime_version`): it relies on plugin connection fields and secrets, the DDL connection context and the bundled Visual EXPLAIN parser.

### Query runtime

- Handle JSON-RPC requests concurrently with a single ordered writer, and cancel the running BigQuery job when Tabularis sends a `cancel` notification.
- Start jobs with a `requestId`, the `client: tabularis` label and a server-side `jobTimeoutMs`; cancel jobs that exceed the query timeout. The default query timeout is now 110 seconds, below the host's 120 second call timeout.
- Retry HTTP 429, 5xx and 403 `rateLimitExceeded`/`backendError` responses with jittered backoff and refresh the token once after HTTP 401. Judge job failure by `status.errorResult`.
- Fill result pages across `pageToken` responses, keep the rows returned by `jobs.query`, and read later pages from the finished job instead of running the SQL again.
- Return TIMESTAMP values with exact microseconds.
- Serve plain `SELECT * FROM table` table browsing through the free `tabledata.list` API.
- Support Tabularis editor sessions with BigQuery sessions, transaction tracking and `release_session`; run stateful batches in a short-lived session.
- Return the last result set and summed DML row count of multi-statement scripts.
- Return Visual EXPLAIN as a raw `bigquery-job-json` payload with a bundled parser: dry-run bytes and on-demand cost estimates, and executed stage plans for EXPLAIN ANALYZE.
- Map common BigQuery error reasons to messages with hints.
- Run metadata queries, DDL, row edits and routine statements through the same job path as editor queries, with typed named query parameters, so they get request IDs, retries, job timeouts, labels, error hints and cancellation.
- Clear the metadata cache after any editor statement other than a plain query, so DDL and DML run from a query tab show up at once.

### Projects and metadata

- Browse several projects from one connection: `get_databases` lists visible projects plus the `additional_projects` setting, and metadata follows the selected project.
- Run and bill every job in the optional `billing_project` (default: the GCP Project ID), while unqualified names and table browsing resolve in the selected project.
- Read routines through the free routines REST API, return host-compatible routine parameters with ordinal positions and a named return row, and add routine management: run, create from templates, edit with rebuilt `CREATE OR REPLACE` scripts (SQL, JavaScript, Python, remote, table and aggregate functions, procedures) and drop.
- Fetch each table resource once with bounded concurrency for schema snapshots, ER diagram batches and AI context, cache metadata briefly, and return all tables from `get_all_columns_batch` and `get_all_foreign_keys_batch`.
- Fill table descriptions, parameterized column types, numeric precision and scale, string lengths and composite foreign-key order.
- Add `get_table_ddl` and use `INFORMATION_SCHEMA.TABLES.ddl` for table, view and materialized view definitions, with a metadata-based fallback that keeps partitioning, clustering and options.
- Keep view options when saving a view and refuse to replace legacy SQL views.
- Refresh materialized views on demand with `BQ.REFRESH_MATERIALIZED_VIEW`.

### DDL and row editing

- Quote every part of generated identifiers and fully qualify objects as `` `project`.`dataset`.`object` ``. Table, column, foreign-key and routine template builders use the browsed project from the connection parameters the host sends with every DDL request, and fall back to dataset-qualified names on older hosts.
- Implement `get_drop_table_sql`, so the explorer drops tables, external tables, snapshots, views and materialized views with the matching statement.
- Declare `column_comments`: column descriptions from the column editor are written with `OPTIONS(description = ...)` on create and add, and changed or cleared with `ALTER COLUMN ... SET OPTIONS(description = ...)`.
- Drop views and materialized views with the matching statement, and reject row edits and column changes on snapshots and external tables.
- Generate only DDL that BigQuery accepts: no `NOT NULL` on added columns, no `SET NOT NULL`, type changes limited to supported widenings, column descriptions through `SET OPTIONS`, primary keys added and dropped as `NOT ENFORCED`, and optional partitioning, clustering and description on new tables.
- Edit rows with typed query parameters; support tables without a primary key by matching comparable columns inside a transaction that rolls back when more than one row matches; report real affected-row counts and explain streaming-buffer errors.
- Write valid GoogleSQL literals for strings with newlines and control characters, NUMERIC, BIGNUMERIC, GEOGRAPHY, INTERVAL, RANGE, JSON, BYTES, ARRAY and STRUCT values.

### Connection and authentication

- Hide unused common connection fields on compatible Tabularis hosts.
- Store plugin-owned credentials in the OS keychain while preserving legacy Password-based connections.
- Author the connection UI in React and TypeScript and generate its release bundle with Vite, with advanced fields for the billing project, additional projects, API endpoint and impersonation.
- Support `impersonated_service_account` ADC files and an optional service account to impersonate, reject service account keys whose `token_uri` is not a googleapis.com endpoint, and tolerate a byte order mark in credentials.
- Add an API endpoint override for Private Service Connect and emulators, and a `none` authentication method for localhost endpoints.
- Fix the client cache key to cover the plugin-owned credential and every connection option, including the billing and data project.
- Use a single `datasets.list` call for `ping`.
- Fix BigQuery REST URLs that contained a double slash after the API version.

### Packaging and tests

- Fix the GCP Project ID placeholder, which suggested the billing project.
- Let `pnpm install --frozen-lockfile` run the esbuild install script with pnpm 10 and later.
- Complete the manifest type mappings, stop requiring precision for NUMERIC and BIGNUMERIC, add registry metadata and declare the Visual EXPLAIN parser.
- Add mocked HTTP, host-shape conformance and BigQuery emulator integration tests for the query runtime and for metadata, DDL and row editing.

## 0.1.0

- Add the initial BigQuery REST API v2 driver implementation.
- Add ADC, service account, authorized user and access token authentication.
- Add dataset, table, view, materialized view, routine and key metadata.
- Add GoogleSQL query, batch, CRUD, DDL, BLOB and Visual EXPLAIN support.
- Add the Tabularis connection UI extension and Tabularium manifest.
- Add cross-platform CI and release packaging.
