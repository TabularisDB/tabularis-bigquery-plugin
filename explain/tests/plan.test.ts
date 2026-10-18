import { readFile } from "node:fs/promises";

import type { ExplainNode } from "@tabularis/explain";
import { describe, expect, it } from "vitest";

import { parseBigQueryJob } from "../src/plan";

async function fixture(name: string): Promise<string> {
  return readFile(new URL(`./fixtures/${name}.json`, import.meta.url), "utf8");
}

function flatten(node: ExplainNode): ExplainNode[] {
  return [node, ...node.children.flatMap(flatten)];
}

describe("parseBigQueryJob", () => {
  it("builds the stage tree from inputStages with inclusive values", async () => {
    const plan = parseBigQueryJob(await fixture("join-analyzed"));

    expect(plan.driver).toBe("bigquery");
    expect(plan.has_analyze_data).toBe(true);
    expect(plan.execution_time_ms).toBe(812);
    expect(plan.root.node_type).toBe("BigQuery job");
    expect(flatten(plan.root).map((node) => node.id)).toEqual([
      "query",
      "stage-3",
      "stage-2",
      "stage-0",
      "stage-1",
    ]);

    const [output] = plan.root.children;
    const join = output.children[0];
    expect(join.join_type).toBe("INNER HASH JOIN EACH WITH ALL");
    expect(join.hash_condition).toBe("$1 = $10");
    // Inclusive: own slot ms plus both inputs.
    expect(join.total_cost).toBe(500 + 600 + 300);
    expect(output.total_cost).toBe(1450);
    expect(plan.root.total_cost).toBe(1450);
    expect(join.actual_time_ms).toBe(400 + 300 + 200);

    const orders = join.children[0];
    expect(orders.relation).toBe("p.sales.orders");
    expect(orders.filter).toBe("greater($2, 100)");
    expect(orders.actual_rows).toBe(2500);
    expect(orders.extra.records_read).toBe(100000);
    expect(orders.extra.shuffle_output).toBe("40.00 KiB");
  });

  it("labels the dry-run cost as an estimate", async () => {
    const plan = parseBigQueryJob(await fixture("estimate"));

    expect(plan.has_analyze_data).toBe(false);
    expect(plan.root.node_type).toBe("Dry run estimate");
    expect(plan.root.extra.bytes_processed).toBe("1.00 TiB");
    expect(plan.root.extra.estimated_cost).toBe("$6.25 (estimate)");
    expect(plan.root.children.map((node) => node.relation)).toEqual(["p.sales.orders"]);
  });

  it("marks cached results that have no stages", () => {
    const plan = parseBigQueryJob(
      JSON.stringify({ version: 1, mode: "analyze", cache_hit: true, query_plan: [] }),
    );
    expect(plan.root.node_type).toBe("BigQuery job (cached result)");
    expect(plan.has_analyze_data).toBe(false);
  });

  it("keeps stages of a malformed cyclic plan", () => {
    const plan = parseBigQueryJob(
      JSON.stringify({
        version: 1,
        mode: "analyze",
        query_plan: [
          { id: "a", name: "A", inputStages: ["b"] },
          { id: "b", name: "B", inputStages: ["a"] },
        ],
      }),
    );
    expect(flatten(plan.root).map((node) => node.node_type).sort()).toEqual([
      "A",
      "B",
      "BigQuery job",
    ]);
  });

  it("rejects payloads it does not understand", () => {
    expect(() => parseBigQueryJob("not json")).toThrow(/Invalid BigQuery plan JSON/);
    expect(() => parseBigQueryJob("[]")).toThrow(/JSON object/);
    expect(() => parseBigQueryJob('{"version":2,"mode":"analyze"}')).toThrow(/version 2/);
    expect(() => parseBigQueryJob('{"version":1,"mode":"other"}')).toThrow(/unknown mode/);
  });

  it("rejects malformed stages and steps with clear errors", () => {
    const plan = (query_plan: unknown) =>
      parseBigQueryJob(JSON.stringify({ version: 1, mode: "analyze", query_plan }));
    expect(() => plan([null])).toThrow(/query_plan\[0\] must be an object/);
    expect(() => plan([{ id: "0" }, 7])).toThrow(/query_plan\[1\] must be an object/);
    expect(() => plan([["stage"]])).toThrow(/query_plan\[0\] must be an object/);
    expect(() => plan([{ id: "0", inputStages: "1" }])).toThrow(/inputStages must be an array/);
    expect(() => plan([{ id: "0", steps: {} }])).toThrow(/steps must be an array/);
    expect(() => plan([{ id: "0", steps: [null] }])).toThrow(/steps\[0\] must be an object/);
    expect(() => plan([{ id: "0", steps: [{ kind: "READ", substeps: "x" }] }])).toThrow(
      /substeps must be an array/,
    );
  });
});
