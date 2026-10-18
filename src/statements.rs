//! Lightweight GoogleSQL statement classification used by the query runtime.
//! It never rewrites SQL; it only decides how a statement is executed.

/// Split a script into statements on top-level semicolons, skipping quoted
/// strings, quoted identifiers and comments.
pub fn split(sql: &str) -> Vec<String> {
    let bytes = sql.as_bytes();
    let mut statements = Vec::new();
    let mut start = 0;
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'\'' | b'"' | b'`' => {
                let quote = bytes[index];
                let triple = index + 2 < bytes.len()
                    && bytes[index + 1] == quote
                    && bytes[index + 2] == quote
                    && quote != b'`';
                index += if triple { 3 } else { 1 };
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index += 2;
                        continue;
                    }
                    if bytes[index] == quote {
                        if !triple {
                            break;
                        }
                        if index + 2 < bytes.len()
                            && bytes[index + 1] == quote
                            && bytes[index + 2] == quote
                        {
                            index += 2;
                            break;
                        }
                    }
                    index += 1;
                }
                index += 1;
            }
            b'-' if bytes.get(index + 1) == Some(&b'-') => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'#' => {
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                while index + 1 < bytes.len() && !(bytes[index] == b'*' && bytes[index + 1] == b'/')
                {
                    index += 1;
                }
                index += 2;
            }
            b';' => {
                push_statement(&mut statements, &sql[start..index]);
                index += 1;
                start = index;
            }
            _ => index += 1,
        }
    }
    if start < sql.len() {
        push_statement(&mut statements, &sql[start.min(sql.len())..]);
    }
    statements
}

fn push_statement(statements: &mut Vec<String>, statement: &str) {
    let statement = strip_comments(statement);
    if !statement.is_empty() {
        statements.push(statement);
    }
}

/// Remove leading comments and surrounding whitespace.
fn strip_comments(statement: &str) -> String {
    let mut rest = statement.trim();
    loop {
        if rest.starts_with("--") || rest.starts_with('#') {
            rest = rest
                .split_once('\n')
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim();
        } else if rest.starts_with("/*") {
            rest = rest
                .split_once("*/")
                .map(|(_, tail)| tail)
                .unwrap_or("")
                .trim();
        } else {
            return rest.to_string();
        }
    }
}

fn words(statement: &str) -> Vec<String> {
    strip_comments(statement)
        .split_whitespace()
        .take(4)
        .map(|word| word.trim_end_matches(';').to_ascii_uppercase())
        .collect()
}

/// Effect of a statement on an explicit session transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionControl {
    Begin,
    End,
    Savepoint,
    None,
}

pub fn transaction_control(statement: &str) -> TransactionControl {
    let words = words(statement);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["SAVEPOINT", ..] | ["RELEASE", "SAVEPOINT", ..] | ["ROLLBACK", "TO", ..] => {
            TransactionControl::Savepoint
        }
        ["BEGIN"] | ["BEGIN", "TRANSACTION"] => TransactionControl::Begin,
        ["COMMIT"] | ["COMMIT", "TRANSACTION"] | ["ROLLBACK"] | ["ROLLBACK", "TRANSACTION"] => {
            TransactionControl::End
        }
        _ => TransactionControl::None,
    }
}

/// Transaction state after running `sql` successfully from `initial`.
pub fn transaction_state_after(sql: &str, initial: bool) -> bool {
    split(sql).iter().fold(initial, |state, statement| {
        match transaction_control(statement) {
            TransactionControl::Begin => true,
            TransactionControl::End => false,
            TransactionControl::Savepoint | TransactionControl::None => state,
        }
    })
}

/// Whether the SQL contains a COMMIT or ROLLBACK, which ends a transaction
/// even when the statement fails.
pub fn ends_transaction(sql: &str) -> bool {
    split(sql)
        .iter()
        .any(|statement| transaction_control(statement) == TransactionControl::End)
}

/// Whether BigQuery may run this SQL as a multi-statement script.
pub fn may_be_script(sql: &str) -> bool {
    let statements = split(sql);
    statements.len() > 1
        || statements.first().is_some_and(|statement| {
            transaction_control(statement) == TransactionControl::None
                && matches!(
                    words(statement).first().map(String::as_str),
                    Some(
                        "DECLARE"
                            | "SET"
                            | "BEGIN"
                            | "IF"
                            | "LOOP"
                            | "WHILE"
                            | "FOR"
                            | "REPEAT"
                            | "CALL"
                            | "EXECUTE"
                    )
                )
        })
}

/// Whether running the SQL again has no side effects, so an expired cached
/// result may be recomputed for a later page.
pub fn is_read_only_query(sql: &str) -> bool {
    let statements = split(sql);
    statements.len() == 1
        && matches!(
            words(&statements[0]).first().map(String::as_str),
            Some("SELECT" | "WITH" | "(")
        )
}

/// Whether a statement creates session state that later statements need.
pub fn needs_session(statement: &str) -> bool {
    let words = words(statement);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    matches!(
        words.as_slice(),
        ["DECLARE", ..]
            | ["SET", ..]
            | ["CREATE", "TEMP", ..]
            | ["CREATE", "TEMPORARY", ..]
            | ["CREATE", "OR", "REPLACE", "TEMP"]
            | ["CREATE", "OR", "REPLACE", "TEMPORARY"]
    ) || transaction_control(statement) != TransactionControl::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_top_level_semicolons_only() {
        let statements =
            split("SELECT ';' AS a; -- c;\nSELECT `x;y` FROM t /* ; */; SELECT \"\"\"a;b\"\"\";");
        assert_eq!(
            statements,
            vec![
                "SELECT ';' AS a",
                "SELECT `x;y` FROM t /* ; */",
                "SELECT \"\"\"a;b\"\"\""
            ]
        );
        assert_eq!(split("  ;; "), Vec::<String>::new());
    }

    #[test]
    fn tracks_transactions() {
        assert_eq!(transaction_control("BEGIN"), TransactionControl::Begin);
        assert_eq!(
            transaction_control("begin transaction;"),
            TransactionControl::Begin
        );
        assert_eq!(transaction_control("COMMIT"), TransactionControl::End);
        assert_eq!(
            transaction_control("ROLLBACK TRANSACTION"),
            TransactionControl::End
        );
        assert_eq!(
            transaction_control("ROLLBACK TO SAVEPOINT tabularis_read"),
            TransactionControl::Savepoint
        );
        assert_eq!(
            transaction_control("SAVEPOINT tabularis_read"),
            TransactionControl::Savepoint
        );
        assert_eq!(
            transaction_control("BEGIN SELECT 1; END"),
            TransactionControl::None
        );
        assert!(transaction_state_after(
            "BEGIN TRANSACTION; UPDATE t SET a = 1",
            false
        ));
        assert!(!transaction_state_after("UPDATE t SET a = 1; COMMIT", true));
        assert!(ends_transaction("-- done\nROLLBACK"));
    }

    #[test]
    fn classifies_scripts_and_rerunnable_reads() {
        assert!(may_be_script("DECLARE x INT64"));
        assert!(may_be_script("SELECT 1; SELECT 2"));
        assert!(!may_be_script("SELECT 1;"));
        assert!(!may_be_script("BEGIN TRANSACTION"));
        assert!(is_read_only_query(
            "  -- note\nWITH a AS (SELECT 1) SELECT * FROM a"
        ));
        assert!(!is_read_only_query("DELETE FROM t WHERE true"));
        assert!(!is_read_only_query("SELECT 1; DELETE FROM t WHERE true"));
    }

    #[test]
    fn detects_statements_that_need_a_session() {
        assert!(needs_session("DECLARE x INT64 DEFAULT 1"));
        assert!(needs_session("create temp table t AS SELECT 1"));
        assert!(needs_session("BEGIN TRANSACTION"));
        assert!(!needs_session("SELECT 1"));
        assert!(!needs_session("CREATE TABLE d.t (a INT64)"));
    }
}
