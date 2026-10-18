import { readFile } from "node:fs/promises";

import {
  registrations,
  type RegisteredExplainParser,
} from "@tabularis/explain";
import { describe, expect, it } from "vitest";

import { bigQueryExplainParser, parseBigQueryJob } from "../src/index";

const fixtureUrl = new URL("./fixtures/join-analyzed.json", import.meta.url);

describe("package entry points", () => {
  it("registers on ESM import and exports the direct parser API", async () => {
    expect(registrations).toEqual([bigQueryExplainParser]);
    expect(bigQueryExplainParser).toMatchObject({
      engine: "bigquery",
      format: "bigquery-job-json",
      label: "BigQuery job plan (JSON)",
      parse: parseBigQueryJob,
    });
    expect(bigQueryExplainParser.sniff?.(await readFile(fixtureUrl, "utf8"))).toBe(true);
    expect(bigQueryExplainParser.sniff?.("<ShowPlanXML/>")).toBe(false);
  });

  it("builds an isolated IIFE descriptor without self-registration", async () => {
    const source = await readFile(new URL("../dist/index.iife.js", import.meta.url), "utf8");
    const registrationsBeforeEvaluation = registrations.length;
    const evaluate = new Function(
      "__TABULARIS_EXPLAIN__",
      `${source}\nreturn typeof __tabularis_explain_parser__ !== "undefined" ? __tabularis_explain_parser__ : null;`,
    );
    const raw = evaluate({}) as Record<string, unknown>;
    const descriptor = (raw.default ?? raw) as RegisteredExplainParser;

    expect(descriptor).toMatchObject({
      engine: "bigquery",
      format: "bigquery-job-json",
      label: "BigQuery job plan (JSON)",
    });
    expect(descriptor.parse(await readFile(fixtureUrl, "utf8")).driver).toBe("bigquery");
    expect(registrations).toHaveLength(registrationsBeforeEvaluation);
  });
});
