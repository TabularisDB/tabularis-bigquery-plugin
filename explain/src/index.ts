import { registerExplainParser } from "@tabularis/explain";

import { bigQueryExplainParser } from "./parser";

registerExplainParser(bigQueryExplainParser);

export { bigQueryExplainParser } from "./parser";
export { parseBigQueryJob } from "./plan";
export type { BigQueryJobPayload, BigQueryStage } from "./plan";
