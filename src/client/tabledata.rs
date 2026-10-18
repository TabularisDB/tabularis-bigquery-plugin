//! Free table browsing through `tabledata.list`.

use serde_json::{json, Value};

use super::transport::CallError;
use super::BigQueryClient;
use crate::bigquery::{fields_from_response, rows_from_response, QueryOutput};
use crate::browse::BrowseTarget;

fn string_u64(value: Option<&Value>) -> Option<u64> {
    value.and_then(|value| match value {
        Value::String(text) => text.parse().ok(),
        other => other.as_u64(),
    })
}

impl BigQueryClient {
    /// Serve a `SELECT * FROM table [LIMIT n [OFFSET m]]` page without a query
    /// job. Returns `Ok(None)` when the table is not a native table or the
    /// server's paging looks inconsistent, so the caller runs a normal job.
    pub(crate) async fn browse_table(
        &self,
        target: &BrowseTarget,
        page: u32,
        page_size: u32,
    ) -> Result<Option<QueryOutput>, CallError> {
        let project = target.project.as_deref().unwrap_or(&self.project_id);
        let table = self
            .get(
                &[
                    "projects",
                    project,
                    "datasets",
                    &target.dataset,
                    "tables",
                    &target.table,
                ],
                &[],
            )
            .await?;
        if table.get("type").and_then(Value::as_str) != Some("TABLE") {
            return Ok(None);
        }
        let stored_rows = string_u64(table.get("numRows")).unwrap_or_default();
        let available = stored_rows.saturating_sub(target.offset);
        let total = target.limit.map_or(available, |limit| limit.min(available));
        let page_offset = u64::from(page.saturating_sub(1)) * u64::from(page_size);
        let wanted = total.saturating_sub(page_offset).min(u64::from(page_size)) as usize;
        let start = target.offset + page_offset;

        let mut rows: Vec<Value> = Vec::new();
        let mut page_token: Option<String> = None;
        while rows.len() < wanted {
            let remaining = wanted - rows.len();
            let mut query = vec![
                ("maxResults".to_string(), remaining.to_string()),
                (
                    "formatOptions.useInt64Timestamp".to_string(),
                    "true".to_string(),
                ),
            ];
            match &page_token {
                Some(token) => query.push(("pageToken".to_string(), token.clone())),
                None => query.push(("startIndex".to_string(), start.to_string())),
            }
            let response = self
                .get(
                    &[
                        "projects",
                        project,
                        "datasets",
                        &target.dataset,
                        "tables",
                        &target.table,
                        "data",
                    ],
                    &query,
                )
                .await?;
            let batch = response
                .get("rows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // A server that ignores startIndex/maxResults (some emulators)
            // returns more rows than asked for; do not trust its pages.
            if batch.len() > remaining {
                return Ok(None);
            }
            page_token = response
                .get("pageToken")
                .and_then(Value::as_str)
                .filter(|token| !token.is_empty())
                .map(str::to_owned);
            if batch.is_empty() {
                break;
            }
            rows.extend(batch);
            if page_token.is_none() {
                break;
            }
        }

        let response =
            json!({"schema": table.get("schema").cloned().unwrap_or(Value::Null), "rows": rows});
        let fields = fields_from_response(&response).map_err(CallError::Other)?;
        let rows = rows_from_response(&response, &fields);
        let has_more = page_offset.saturating_add(rows.len() as u64) < total;
        Ok(Some(QueryOutput {
            columns: fields.iter().map(|field| field.name.clone()).collect(),
            fields,
            rows,
            affected_rows: 0,
            total_rows: Some(total),
            has_more,
            total_bytes_processed: Some("0".to_string()),
            cache_hit: None,
            job_id: None,
            source: Some("tabledata.list".to_string()),
        }))
    }
}
