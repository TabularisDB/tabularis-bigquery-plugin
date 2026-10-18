//! Recognition of plain table-browsing queries.
//!
//! Tabularis opens a table with `SELECT * FROM `dataset`.`table`` and an
//! optional `LIMIT`. Such a query can be served by `tabledata.list`, which is
//! free and scans nothing. The match is deliberately exact: any WHERE, ORDER
//! BY, column list, wildcard table or decorator falls back to a query job.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowseTarget {
    pub project: Option<String>,
    pub dataset: String,
    pub table: String,
    pub limit: Option<u64>,
    pub offset: u64,
}

pub fn parse(sql: &str, default_dataset: Option<&str>) -> Option<BrowseTarget> {
    let mut rest = sql.trim();
    rest = rest.strip_suffix(';').unwrap_or(rest).trim_end();
    rest = keyword(rest, "SELECT")?;
    rest = rest.strip_prefix('*')?.trim_start();
    rest = keyword(rest, "FROM")?;
    let (parts, mut rest) = table_reference(rest)?;
    rest = rest.trim_start();

    let mut limit = None;
    let mut offset = 0;
    if !rest.is_empty() {
        rest = keyword(rest, "LIMIT")?;
        let (value, tail) = number(rest)?;
        limit = Some(value);
        rest = tail.trim_start();
        if !rest.is_empty() {
            rest = keyword(rest, "OFFSET")?;
            let (value, tail) = number(rest)?;
            offset = value;
            rest = tail.trim_start();
        }
    }
    if !rest.is_empty() {
        return None;
    }
    if parts
        .iter()
        .any(|part| part.eq_ignore_ascii_case("INFORMATION_SCHEMA") || part.starts_with("region-"))
    {
        return None;
    }

    let (project, dataset, table) = match parts.as_slice() {
        [table] => (
            None,
            default_dataset
                .filter(|dataset| !dataset.is_empty())?
                .to_string(),
            table.clone(),
        ),
        [dataset, table] => (None, dataset.clone(), table.clone()),
        [project, dataset, table] => (Some(project.clone()), dataset.clone(), table.clone()),
        _ => return None,
    };
    if !valid_dataset(&dataset) {
        return None;
    }
    Some(BrowseTarget {
        project,
        dataset,
        table,
        limit,
        offset,
    })
}

/// Consume a case-insensitive keyword followed by whitespace.
fn keyword<'a>(input: &'a str, word: &str) -> Option<&'a str> {
    let head = input.get(..word.len())?;
    let tail = &input[word.len()..];
    (head.eq_ignore_ascii_case(word) && tail.starts_with(char::is_whitespace))
        .then(|| tail.trim_start())
}

fn number(input: &str) -> Option<(u64, &str)> {
    let end = input
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(input.len());
    if end == 0 {
        return None;
    }
    Some((input[..end].parse().ok()?, &input[end..]))
}

/// Parse `a.b.c`, `` `a`.`b` `` or `` `a.b` `` into its path parts.
fn table_reference(input: &str) -> Option<(Vec<String>, &str)> {
    let mut parts = Vec::new();
    let mut rest = input;
    loop {
        if let Some(quoted) = rest.strip_prefix('`') {
            let end = quoted.find('`')?;
            let content = &quoted[..end];
            if content.is_empty()
                || content.contains(['\\', '*', '$', '@', '{', '}'])
                || content.trim() != content
            {
                return None;
            }
            parts.extend(content.split('.').map(str::to_owned));
            rest = &quoted[end + 1..];
        } else {
            let end = rest
                .find(|character: char| !(character.is_ascii_alphanumeric() || character == '_'))
                .unwrap_or(rest.len());
            let name = &rest[..end];
            if name.is_empty() || name.starts_with(|character: char| character.is_ascii_digit()) {
                return None;
            }
            parts.push(name.to_string());
            rest = &rest[end..];
        }
        match rest.strip_prefix('.') {
            Some(tail) => rest = tail,
            None => break,
        }
    }
    if parts.iter().any(String::is_empty) || parts.len() > 3 {
        return None;
    }
    // Reject anything glued to the reference, such as an alias or a comma.
    if !(rest.is_empty() || rest.starts_with(char::is_whitespace)) {
        return None;
    }
    Some((parts, rest))
}

fn valid_dataset(dataset: &str) -> bool {
    !dataset.is_empty()
        && dataset
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

#[cfg(test)]
mod tests {
    use super::{parse, BrowseTarget};

    fn target(project: Option<&str>, dataset: &str, table: &str) -> BrowseTarget {
        BrowseTarget {
            project: project.map(str::to_owned),
            dataset: dataset.into(),
            table: table.into(),
            limit: None,
            offset: 0,
        }
    }

    #[test]
    fn matches_host_table_open_queries() {
        assert_eq!(
            parse("SELECT * FROM `sales`.`orders`", None),
            Some(target(None, "sales", "orders"))
        );
        assert_eq!(
            parse("select * from `my-project.sales.orders` LIMIT 100;", None),
            Some(BrowseTarget {
                limit: Some(100),
                ..target(Some("my-project"), "sales", "orders")
            })
        );
        assert_eq!(
            parse("SELECT * FROM sales.orders LIMIT 10 OFFSET 20", None),
            Some(BrowseTarget {
                limit: Some(10),
                offset: 20,
                ..target(None, "sales", "orders")
            })
        );
        assert_eq!(
            parse("SELECT * FROM `orders`", Some("sales")),
            Some(target(None, "sales", "orders"))
        );
        assert_eq!(
            parse("SELECT * FROM `sales`.`order items`", None),
            Some(target(None, "sales", "order items"))
        );
    }

    #[test]
    fn rejects_everything_else() {
        for sql in [
            "SELECT * FROM `orders`",
            "SELECT * FROM sales.orders WHERE id = 1",
            "SELECT * FROM sales.orders ORDER BY id LIMIT 10",
            "SELECT id FROM sales.orders",
            "SELECT * EXCEPT (id) FROM sales.orders",
            "SELECT * FROM `sales.events_*`",
            "SELECT * FROM `sales.events$20240101`",
            "SELECT * FROM sales.INFORMATION_SCHEMA.TABLES",
            "SELECT * FROM `region-us`.INFORMATION_SCHEMA.JOBS",
            "SELECT * FROM sales.orders o",
            "SELECT * FROM sales.orders, sales.items",
            "SELECT * FROM (SELECT * FROM `sales`.`orders` LIMIT 10) AS limited_subset",
            "SELECT * FROM sales.orders LIMIT @n",
            "SELECT * FROM sales.orders; DELETE FROM sales.orders WHERE true",
            "SELECT * FROM a.b.c.d",
            "SELECT * FROM `sales`.`orders` FOR SYSTEM_TIME AS OF TIMESTAMP '2024-01-01'",
            "SELECT * FROM sales.orders TABLESAMPLE SYSTEM (10 PERCENT)",
        ] {
            assert_eq!(parse(sql, None), None, "{sql}");
        }
    }
}
