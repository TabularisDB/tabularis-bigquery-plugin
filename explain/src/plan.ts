import type { ExplainNode, ExplainPlan } from "@tabularis/explain";

/** One `statistics.query.queryPlan` entry. BigQuery encodes int64 as strings. */
export interface BigQueryStage {
  id?: string;
  name?: string;
  status?: string;
  inputStages?: string[];
  startMs?: string;
  endMs?: string;
  slotMs?: string;
  recordsRead?: string;
  recordsWritten?: string;
  parallelInputs?: string;
  completedParallelInputs?: string;
  shuffleOutputBytes?: string;
  shuffleOutputBytesSpilled?: string;
  waitMsAvg?: string;
  waitMsMax?: string;
  readMsAvg?: string;
  readMsMax?: string;
  computeMsAvg?: string;
  computeMsMax?: string;
  writeMsAvg?: string;
  writeMsMax?: string;
  steps?: { kind?: string; substeps?: string[] }[];
}

/** Payload written by the plugin's `explain_query` (`src/explain.rs`). */
export interface BigQueryJobPayload {
  version: number;
  mode: "estimate" | "analyze";
  job_id?: string | null;
  location?: string | null;
  statement_type?: string | null;
  total_bytes_processed?: number | null;
  total_bytes_processed_accuracy?: string | null;
  total_bytes_billed?: number | null;
  estimated_cost_usd?: number | null;
  cost_note?: string | null;
  cache_hit?: boolean | null;
  total_slot_ms?: number | null;
  elapsed_ms?: number | null;
  billing_tier?: number | null;
  referenced_tables?: string[];
  query_plan?: BigQueryStage[];
}

function int(value: string | number | null | undefined): number | null {
  if (value === null || value === undefined || value === "") return null;
  const parsed = typeof value === "number" ? value : Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function emptyNode(id: string, nodeType: string): ExplainNode {
  return {
    id,
    node_type: nodeType,
    relation: null,
    startup_cost: null,
    total_cost: null,
    plan_rows: null,
    actual_rows: null,
    actual_time_ms: null,
    actual_loops: null,
    buffers_hit: null,
    buffers_read: null,
    filter: null,
    index_condition: null,
    join_type: null,
    hash_condition: null,
    extra: {},
    children: [],
  };
}

function formatBytes(bytes: number): string {
  const units = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return unit === 0 ? `${bytes} B` : `${value.toFixed(2)} ${units[unit]}`;
}

function formatUsd(value: number): string {
  if (value === 0) return "$0.00";
  if (value < 0.01) return `$${value.toPrecision(2)}`;
  return `$${value.toFixed(2)}`;
}

function substeps(stage: BigQueryStage, kind: string): string[] {
  return (stage.steps ?? [])
    .filter((step) => step.kind === kind)
    .flatMap((step) => step.substeps ?? []);
}

/** Tables a stage reads, ignoring intermediate stage outputs. */
function relationOf(stage: BigQueryStage): string | null {
  const tables = substeps(stage, "READ")
    .filter((substep) => substep.startsWith("FROM "))
    .map((substep) => substep.slice(5).trim())
    .filter((source) => !source.startsWith("__stage") && !source.startsWith("$"));
  return tables.length > 0 ? tables.join(", ") : null;
}

function filterOf(stage: BigQueryStage): string | null {
  const filters = [...substeps(stage, "READ"), ...substeps(stage, "FILTER")]
    .filter((substep) => substep.startsWith("WHERE "))
    .map((substep) => substep.slice(6).trim());
  return filters.length > 0 ? filters.join(" AND ") : null;
}

function joinOf(stage: BigQueryStage): { type: string | null; condition: string | null } {
  const join = substeps(stage, "JOIN")[0];
  if (join === undefined) return { type: null, condition: null };
  const on = join.indexOf(" ON ");
  return on === -1
    ? { type: join, condition: null }
    : { type: join.slice(0, on).trim(), condition: join.slice(on + 4).trim() };
}

function stageNode(stage: BigQueryStage, key: string): ExplainNode {
  const node = emptyNode(`stage-${key}`, stage.name ?? `Stage ${key}`);
  const start = int(stage.startMs);
  const end = int(stage.endMs);
  const join = joinOf(stage);
  node.relation = relationOf(stage);
  node.filter = filterOf(stage);
  node.join_type = join.type;
  node.hash_condition = join.condition;
  node.actual_rows = int(stage.recordsWritten);
  node.actual_time_ms = start !== null && end !== null && end >= start ? end - start : null;
  node.total_cost = int(stage.slotMs);
  const shuffle = int(stage.shuffleOutputBytes);
  const spilled = int(stage.shuffleOutputBytesSpilled);
  node.extra = {
    status: stage.status ?? null,
    slot_ms: int(stage.slotMs),
    records_read: int(stage.recordsRead),
    records_written: int(stage.recordsWritten),
    parallel_inputs: int(stage.parallelInputs),
    completed_parallel_inputs: int(stage.completedParallelInputs),
    shuffle_output: shuffle === null ? null : formatBytes(shuffle),
    shuffle_spilled: spilled === null ? null : formatBytes(spilled),
    wait_ms_avg: int(stage.waitMsAvg),
    wait_ms_max: int(stage.waitMsMax),
    read_ms_avg: int(stage.readMsAvg),
    read_ms_max: int(stage.readMsMax),
    compute_ms_avg: int(stage.computeMsAvg),
    compute_ms_max: int(stage.computeMsMax),
    write_ms_avg: int(stage.writeMsAvg),
    write_ms_max: int(stage.writeMsMax),
    steps: (stage.steps ?? []).map(
      (step) => `${step.kind ?? "STEP"}: ${(step.substeps ?? []).join("; ")}`,
    ),
  };
  return node;
}

/**
 * Turn per-node own values into the inclusive values Visual EXPLAIN expects
 * (a node's cost and time cover its children, as in PostgreSQL plans).
 */
function accumulate(node: ExplainNode): void {
  let childCost = 0;
  let childTime = 0;
  let anyCost = false;
  let anyTime = false;
  for (const child of node.children) {
    accumulate(child);
    if (child.total_cost !== null) {
      childCost += child.total_cost;
      anyCost = true;
    }
    if (child.actual_time_ms !== null) {
      childTime += child.actual_time_ms;
      anyTime = true;
    }
  }
  if (anyCost) node.total_cost = (node.total_cost ?? 0) + childCost;
  if (anyTime) node.actual_time_ms = (node.actual_time_ms ?? 0) + childTime;
}

/**
 * Build the stage tree. BigQuery stages form a DAG through `inputStages`; a
 * stage feeding several consumers is attached to the first one and listed in
 * the others' `extra.also_reads_stages`.
 */
function stageTree(stages: BigQueryStage[]): ExplainNode[] {
  const byId = new Map<string, BigQueryStage>();
  stages.forEach((stage, index) => byId.set(stage.id ?? String(index), stage));
  const parentOf = new Map<string, string>();
  const shared = new Map<string, string[]>();
  for (const [id, stage] of byId) {
    for (const input of stage.inputStages ?? []) {
      if (!byId.has(input) || input === id) continue;
      if (parentOf.has(input)) {
        shared.set(id, [...(shared.get(id) ?? []), input]);
      } else {
        parentOf.set(input, id);
      }
    }
  }

  const visited = new Set<string>();
  const build = (id: string): ExplainNode => {
    visited.add(id);
    const node = stageNode(byId.get(id)!, id);
    const alsoReads = shared.get(id);
    if (alsoReads) node.extra.also_reads_stages = alsoReads;
    for (const input of byId.get(id)!.inputStages ?? []) {
      if (parentOf.get(input) === id && !visited.has(input)) {
        node.children.push(build(input));
      }
    }
    return node;
  };

  const roots = [...byId.keys()].filter((id) => !parentOf.has(id));
  const tree = roots.map(build);
  // A malformed cycle leaves stages unvisited; keep them visible at the top.
  for (const id of byId.keys()) {
    if (!visited.has(id)) tree.push(build(id));
  }
  return tree.reverse();
}

function parsePayload(payload: string): BigQueryJobPayload {
  let value: unknown;
  try {
    value = JSON.parse(payload);
  } catch (error) {
    throw new Error(`Invalid BigQuery plan JSON: ${(error as Error).message}`);
  }
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    throw new Error("BigQuery plan payload must be a JSON object");
  }
  const job = value as BigQueryJobPayload;
  if (job.version !== 1) {
    throw new Error(`Unsupported BigQuery plan payload version ${String(job.version)}`);
  }
  if (job.mode !== "estimate" && job.mode !== "analyze") {
    throw new Error("BigQuery plan payload has an unknown mode");
  }
  if (job.query_plan !== undefined && !Array.isArray(job.query_plan)) {
    throw new Error("BigQuery plan payload query_plan must be an array");
  }
  return job;
}

/** Parse the `bigquery-job-json` payload into a Visual EXPLAIN plan. */
export function parseBigQueryJob(payload: string): ExplainPlan {
  const job = parsePayload(payload);
  const analyze = job.mode === "analyze";
  const root = emptyNode("query", analyze ? "BigQuery job" : "Dry run estimate");
  const processed = int(job.total_bytes_processed);
  const billed = int(job.total_bytes_billed);
  const cost = int(job.estimated_cost_usd);
  root.extra = {
    statement_type: job.statement_type ?? null,
    bytes_processed: processed === null ? null : formatBytes(processed),
    bytes_processed_exact: processed,
    bytes_processed_accuracy: job.total_bytes_processed_accuracy ?? null,
    bytes_billed: billed === null ? null : formatBytes(billed),
    estimated_cost: cost === null ? null : `${formatUsd(cost)} (estimate)`,
    cost_note: job.cost_note ?? null,
    cache_hit: job.cache_hit ?? null,
    slot_ms: int(job.total_slot_ms),
    billing_tier: int(job.billing_tier),
    job_id: job.job_id ?? null,
    location: job.location ?? null,
  };

  if (analyze) {
    root.children = stageTree(job.query_plan ?? []);
    accumulate(root);
    if (root.children.length === 0 && job.cache_hit) {
      root.node_type = "BigQuery job (cached result)";
    }
  } else {
    root.children = (job.referenced_tables ?? []).map((table, index) => {
      const node = emptyNode(`table-${index}`, "Referenced table");
      node.relation = table;
      return node;
    });
  }

  return {
    root,
    planning_time_ms: null,
    execution_time_ms: analyze ? int(job.elapsed_ms) : null,
    original_query: "",
    driver: "bigquery",
    has_analyze_data: analyze && (job.query_plan ?? []).length > 0,
    raw_output: JSON.stringify(job, null, 2),
  };
}
