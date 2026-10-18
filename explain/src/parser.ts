import type { RegisteredExplainParser } from "@tabularis/explain";

import { parseBigQueryJob } from "./plan";

/** BigQuery parser descriptor consumed by both package and plugin loaders. */
export const bigQueryExplainParser: RegisteredExplainParser = {
  engine: "bigquery",
  format: "bigquery-job-json",
  label: "BigQuery job plan (JSON)",
  parse: parseBigQueryJob,
  // Cheap check for the plugin payload; the parser still validates fully.
  sniff: (payload) => {
    const head = payload.slice(0, 4096);
    return /^\s*\{/.test(head) && /"query_plan"\s*:/.test(payload) && /"mode"\s*:\s*"(estimate|analyze)"/.test(head);
  },
};
