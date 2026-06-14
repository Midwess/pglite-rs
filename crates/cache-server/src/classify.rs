use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions, visit_relations, BinaryOperator, Expr, SetExpr, Statement, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::error::CacheError;

pub struct CacheableQuery {
    pub fingerprint: u64,
    pub tables: Vec<String>,
    pub eq_filters: Vec<(String, String)>,
    pub sql: String,
}

const SIDE_EFFECTING: &[&str] = &[
    "nextval",
    "setval",
    "pg_advisory_lock",
    "pg_advisory_xact_lock",
    "pg_try_advisory_lock",
    "pg_advisory_unlock",
];

const VOLATILE_READ: &[&str] = &[
    "now",
    "random",
    "random_normal",
    "clock_timestamp",
    "timeofday",
    "statement_timestamp",
    "gen_random_uuid",
    "uuid_generate_v4",
];

pub struct ReadClassifier {
    replicated: HashSet<String>,
}

impl ReadClassifier {
    pub fn new(replicated: HashSet<String>) -> ReadClassifier {
        ReadClassifier {
            replicated: replicated.into_iter().map(|t| bare_lower(&t)).collect(),
        }
    }

    pub fn classify(&self, sql: &str) -> Result<CacheableQuery, CacheError> {
        let statements = Parser::parse_sql(&PostgreSqlDialect {}, sql).map_err(|_| {
            CacheError::Rejected(
                "could not parse as a read-only SELECT; this server caches read queries only"
                    .to_string(),
            )
        })?;

        if statements.len() != 1 {
            return Err(CacheError::Rejected(
                "only a single SELECT statement is supported".to_string(),
            ));
        }

        let statement = &statements[0];
        let query =
            match statement {
                Statement::Query(query) => query,
                _ => return Err(CacheError::Rejected(
                    "only read-only SELECT queries are cacheable; writes and DDL are not supported"
                        .to_string(),
                )),
            };

        if !query.locks.is_empty() {
            return Err(CacheError::Rejected(
                "locking reads (FOR UPDATE / FOR SHARE) are not supported on a read-only cache"
                    .to_string(),
            ));
        }
        if let SetExpr::Select(select) = query.body.as_ref() {
            if select.into.is_some() {
                return Err(CacheError::Rejected(
                    "SELECT INTO is not supported because it writes".to_string(),
                ));
            }
        }

        let mut cte_names = HashSet::new();
        if let Some(with) = &query.with {
            for cte in &with.cte_tables {
                cte_names.insert(bare_lower(&cte.alias.name.value));
            }
        }

        let mut functions = Vec::new();
        let _ = visit_expressions(statement, |expr| {
            if let Expr::Function(function) = expr {
                functions.push(bare_lower(&function.name.to_string()));
            }
            ControlFlow::<()>::Continue(())
        });
        if let Some(name) = functions
            .iter()
            .find(|f| SIDE_EFFECTING.contains(&f.as_str()))
        {
            return Err(CacheError::Rejected(format!(
                "function `{name}` has side effects and cannot be cached"
            )));
        }
        if let Some(name) = functions
            .iter()
            .find(|f| VOLATILE_READ.contains(&f.as_str()))
        {
            return Err(CacheError::Rejected(format!(
                "non-deterministic function `{name}` cannot be cached or kept realtime"
            )));
        }

        let mut tables = Vec::new();
        let _ = visit_relations(statement, |name| {
            let table = bare_lower(&name.to_string());
            if !cte_names.contains(&table) && !tables.contains(&table) {
                tables.push(table);
            }
            ControlFlow::<()>::Continue(())
        });
        for table in &tables {
            if !self.replicated.contains(table) {
                return Err(CacheError::Rejected(format!(
                    "table `{table}` is not available in this cache (not replicated)"
                )));
            }
        }

        let mut eq_filters = Vec::new();
        if let SetExpr::Select(select) = query.body.as_ref() {
            if let Some(selection) = &select.selection {
                collect_eq_filters(selection, &mut eq_filters);
            }
        }

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        statement.to_string().hash(&mut hasher);

        Ok(CacheableQuery {
            fingerprint: hasher.finish(),
            tables,
            eq_filters,
            sql: sql.to_string(),
        })
    }
}

fn collect_eq_filters(root: &Expr, out: &mut Vec<(String, String)>) {
    let mut stack = vec![root];
    while let Some(expr) = stack.pop() {
        match expr {
            Expr::BinaryOp {
                left,
                op: BinaryOperator::And,
                right,
            } => {
                stack.push(left.as_ref());
                stack.push(right.as_ref());
            }
            Expr::BinaryOp {
                left,
                op: BinaryOperator::Eq,
                right,
            } => {
                if let (Some(column), Some(value)) = (ident_name(left), literal_value(right)) {
                    out.push((column, value));
                } else if let (Some(column), Some(value)) = (ident_name(right), literal_value(left))
                {
                    out.push((column, value));
                }
            }
            Expr::Nested(inner) => stack.push(inner.as_ref()),
            _ => {}
        }
    }
}

fn ident_name(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Identifier(ident) => Some(ident.value.to_ascii_lowercase()),
        Expr::CompoundIdentifier(parts) => parts.last().map(|part| part.value.to_ascii_lowercase()),
        _ => None,
    }
}

fn literal_value(expr: &Expr) -> Option<String> {
    match expr {
        Expr::Value(Value::Number(number, _)) => Some(number.clone()),
        Expr::Value(Value::SingleQuotedString(text)) => Some(text.clone()),
        Expr::Value(Value::Boolean(flag)) => Some(flag.to_string()),
        _ => None,
    }
}

fn bare_lower(name: &str) -> String {
    name.rsplit('.').next().unwrap_or(name).to_ascii_lowercase()
}
