//! Raw EXPLAIN payloads for the `bigquery-job-json` parser bundle.
//!
//! The plugin hands the host a JSON document built from a dry-run or finished
//! job; `explain/src` turns it into a Visual EXPLAIN tree.

use serde_json::{json, Value};

pub const ENGINE: &str = "bigquery";
pub const FORMAT: &str = "bigquery-job-json";
/// On-demand list price used for the estimate, in US dollars per TiB.
pub const ON_DEMAND_USD_PER_TIB: f64 = 6.25;
const TIB: f64 = 1_099_511_627_776.0;

pub const COST_NOTE: &str = "Estimate at the on-demand list price of $6.25 per TiB. Actual cost depends on your pricing model (editions or reservations), free tier, regional pricing and the 10 MiB minimum per referenced table.";

fn number(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| match value {
        Value::String(text) => text.parse().ok(),
        other => other.as_u64(),
    })
}

pub fn estimated_cost_usd(bytes: u64) -> f64 {
    bytes as f64 / TIB * ON_DEMAND_USD_PER_TIB
}

/// Build the payload from a job resource. `mode` is `estimate` for a dry run
/// and `analyze` for an executed job.
pub fn payload(mode: &str, job: &Value) -> Value {
    let statistics = job.get("statistics").cloned().unwrap_or(Value::Null);
    let query = statistics.get("query").cloned().unwrap_or(Value::Null);
    let processed = number(query.get("totalBytesProcessed"))
        .or_else(|| number(statistics.get("totalBytesProcessed")));
    let billed = number(query.get("totalBytesBilled"));
    let cost_bytes = if mode == "analyze" {
        billed.or(processed)
    } else {
        processed
    };
    let elapsed_ms = match (
        number(statistics.get("startTime")),
        number(statistics.get("endTime")),
    ) {
        (Some(start), Some(end)) if end >= start => Some(end - start),
        _ => None,
    };
    let referenced_tables: Vec<String> = query
        .get("referencedTables")
        .and_then(Value::as_array)
        .map(|tables| {
            tables
                .iter()
                .filter_map(|table| {
                    Some(format!(
                        "{}.{}.{}",
                        table.get("projectId")?.as_str()?,
                        table.get("datasetId")?.as_str()?,
                        table.get("tableId")?.as_str()?
                    ))
                })
                .collect()
        })
        .unwrap_or_default();

    json!({
        "version": 1,
        "mode": mode,
        "job_id": job.pointer("/jobReference/jobId"),
        "location": job.pointer("/jobReference/location"),
        "statement_type": query.get("statementType"),
        "total_bytes_processed": processed,
        "total_bytes_processed_accuracy": query.get("totalBytesProcessedAccuracy"),
        "total_bytes_billed": billed,
        "estimated_cost_usd": cost_bytes.map(estimated_cost_usd),
        "cost_note": COST_NOTE,
        "cache_hit": query.get("cacheHit"),
        "total_slot_ms": number(query.get("totalSlotMs")).or_else(|| number(statistics.get("totalSlotMs"))),
        "elapsed_ms": elapsed_ms,
        "billing_tier": query.get("billingTier"),
        "referenced_tables": referenced_tables,
        "query_plan": query.get("queryPlan").cloned().unwrap_or_else(|| json!([])),
        "timeline": query.get("timeline").cloned().unwrap_or_else(|| json!([]))
    })
}

/// The raw `explain_query` result recognised by Tabularis 0.23.0 and later.
pub fn raw_output(payload: &Value, original_query: &str) -> Value {
    json!({
        "engine": ENGINE,
        "format": FORMAT,
        "payload": payload.to_string(),
        "original_query": original_query
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{estimated_cost_usd, payload};

    #[test]
    fn estimates_on_demand_cost_per_tib() {
        assert!((estimated_cost_usd(1_099_511_627_776) - 6.25).abs() < 1e-9);
        assert_eq!(estimated_cost_usd(0), 0.0);
    }

    #[test]
    fn summarizes_dry_runs_and_executed_jobs() {
        let job = json!({
            "jobReference": {"jobId": "j", "location": "US"},
            "statistics": {
                "startTime": "1000", "endTime": "1600",
                "query": {
                    "statementType": "SELECT",
                    "totalBytesProcessed": "2199023255552",
                    "totalBytesBilled": "1099511627776",
                    "cacheHit": false,
                    "totalSlotMs": "42",
                    "referencedTables": [{"projectId": "p", "datasetId": "d", "tableId": "t"}],
                    "queryPlan": [{"id": "0", "name": "S00: Input"}]
                }
            }
        });
        let estimate = payload("estimate", &job);
        assert_eq!(estimate["estimated_cost_usd"], 12.5);
        assert_eq!(estimate["referenced_tables"], json!(["p.d.t"]));
        let analyzed = payload("analyze", &job);
        assert_eq!(analyzed["estimated_cost_usd"], 6.25);
        assert_eq!(analyzed["elapsed_ms"], 600);
        assert_eq!(analyzed["total_slot_ms"], 42);
        assert_eq!(analyzed["query_plan"][0]["name"], "S00: Input");
    }
}
