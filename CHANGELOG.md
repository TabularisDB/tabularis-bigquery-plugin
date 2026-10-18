# Changelog

All notable changes to this project are documented in this file.

## 0.2.0

- Hide unused common connection fields on compatible Tabularis hosts.
- Store plugin-owned credentials in the OS keychain while preserving legacy Password-based connections.
- Author the connection UI in React and TypeScript and generate its release bundle with Vite.
- Browse several projects from one connection: `get_databases` lists visible projects plus the `additional_projects` setting, metadata follows the selected project, and jobs run in the configured or `billing_project` project.
- Quote every part of generated identifiers and fully qualify objects as `` `project`.`dataset`.`object` ``.
- Read routines through the free routines REST API, return host-compatible routine parameters with ordinal positions and a named return row, and add routine management: run, create from templates, edit with rebuilt `CREATE OR REPLACE` scripts (SQL, JavaScript, Python, remote, table and aggregate functions, procedures) and drop.
- Fetch each table resource once with bounded concurrency for schema snapshots, ER diagram batches and AI context, cache metadata briefly, and return all tables from `get_all_columns_batch` and `get_all_foreign_keys_batch`.
- Fill table descriptions, parameterized column types, numeric precision and scale, string lengths and composite foreign-key order.
- Add `get_table_ddl` and use `INFORMATION_SCHEMA.TABLES.ddl` for table, view and materialized view definitions, with a metadata-based fallback that keeps partitioning, clustering and options.
- Keep view options when saving a view and refuse to replace legacy SQL views.
- Refresh materialized views on demand with `BQ.REFRESH_MATERIALIZED_VIEW`.
- Drop views and materialized views with the matching statement, and reject row edits and column changes on snapshots and external tables.
- Generate only DDL that BigQuery accepts: no `NOT NULL` on added columns, no `SET NOT NULL`, type changes limited to supported widenings, column descriptions through `SET OPTIONS`, primary keys added and dropped as `NOT ENFORCED`, and optional partitioning, clustering and description on new tables.
- Edit rows with typed query parameters; support tables without a primary key by matching comparable columns inside a transaction that rolls back when more than one row matches; report real affected-row counts and explain streaming-buffer errors.
- Write valid GoogleSQL literals for strings with newlines and control characters, NUMERIC, BIGNUMERIC, GEOGRAPHY, INTERVAL, RANGE, JSON, BYTES, ARRAY and STRUCT values.
- Fix BigQuery REST URLs that contained a double slash after the API version.
- Complete the manifest type mappings, stop requiring precision for NUMERIC and BIGNUMERIC, and add registry metadata.

## 0.1.0

- Add the initial BigQuery REST API v2 driver implementation.
- Add ADC, service account, authorized user and access token authentication.
- Add dataset, table, view, materialized view, routine and key metadata.
- Add GoogleSQL query, batch, CRUD, DDL, BLOB and Visual EXPLAIN support.
- Add the Tabularis connection UI extension and Tabularium manifest.
- Add cross-platform CI and release packaging.
