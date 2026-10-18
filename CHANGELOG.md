# Changelog

All notable changes to this project are documented in this file.

## Unreleased

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
