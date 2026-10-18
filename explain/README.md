# `@tabularis/explain-bigquery`

Google BigQuery job plan parser for Tabularis Visual EXPLAIN. It is maintained
and versioned with the
[Tabularis BigQuery plugin](https://github.com/TabularisDB/tabularis-bigquery-plugin),
which ships `dist/index.iife.js` in each release archive.

The parser reads the `bigquery-job-json` payload produced by the plugin's
`explain_query`:

- `mode: "estimate"`: a dry run. The plan shows bytes processed, an on-demand
  cost estimate and the referenced tables. Nothing is executed.
- `mode: "analyze"`: an executed job. Each query stage becomes a node with slot
  milliseconds as cost, stage wall time, records read and written, shuffle
  bytes and the stage steps.

`@tabularis/explain` 0.2.0 or newer is required for the parser registry.

```ts
import "@tabularis/explain-bigquery";
import { parseRawExplain } from "@tabularis/explain";

const plan = parseRawExplain({
  engine: "bigquery",
  format: "bigquery-job-json",
  payload,
  original_query: "SELECT * FROM sales.orders",
});
```

## Development

```bash
pnpm install
pnpm typecheck
pnpm test
pnpm build
```
