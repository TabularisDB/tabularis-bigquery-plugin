# Changelog

All notable changes to this project are documented in this file.

## Unreleased

### 0.2.0 query runtime

- Handle JSON-RPC requests concurrently with a single ordered writer, and cancel the running BigQuery job when Tabularis sends a `cancel` notification.
- Start jobs with a `requestId`, the `client: tabularis` label and a server-side `jobTimeoutMs`; cancel jobs that exceed the query timeout. The default query timeout is now 110 seconds, below the host's 120 second call timeout.
- Retry HTTP 429, 5xx and 403 `rateLimitExceeded`/`backendError` responses with jittered backoff and refresh the token once after HTTP 401. Judge job failure by `status.errorResult`.
- Fill result pages across `pageToken` responses, keep the rows returned by `jobs.query`, and read later pages from the finished job instead of running the SQL again.
- Return TIMESTAMP values with exact microseconds.
- Serve plain `SELECT * FROM table` table browsing through the free `tabledata.list` API.
- Support Tabularis editor sessions with BigQuery sessions, transaction tracking and `release_session`; run stateful batches in a short-lived session.
- Return the last result set and summed DML row count of multi-statement scripts.
- Return Visual EXPLAIN as a raw `bigquery-job-json` payload with a bundled parser: dry-run bytes and on-demand cost estimates, and executed stage plans for EXPLAIN ANALYZE. Requires Tabularis 0.23.0.
- Map common BigQuery error reasons to messages with hints.
- Support `impersonated_service_account` ADC files and an optional service account to impersonate, reject service account keys whose `token_uri` is not a googleapis.com endpoint, and tolerate a byte order mark in credentials.
- Add an API endpoint override for Private Service Connect and emulators, and a `none` authentication method for localhost endpoints.
- Fix the client cache key to cover the plugin-owned credential and every connection option.
- Use a single `datasets.list` call for `ping`.
- Add mocked HTTP, host-shape conformance and BigQuery emulator integration tests.

- Hide unused common connection fields on compatible Tabularis hosts.
- Store plugin-owned credentials in the OS keychain while preserving legacy Password-based connections.
- Author the connection UI in React and TypeScript and generate its release bundle with Vite.

## 0.1.0

- Add the initial BigQuery REST API v2 driver implementation.
- Add ADC, service account, authorized user and access token authentication.
- Add dataset, table, view, materialized view, routine and key metadata.
- Add GoogleSQL query, batch, CRUD, DDL, BLOB and Visual EXPLAIN support.
- Add the Tabularis connection UI extension and Tabularium manifest.
- Add cross-platform CI and release packaging.
