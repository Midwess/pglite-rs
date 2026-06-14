use std::collections::HashSet;
use std::hash::{Hash, Hasher};
use std::ops::ControlFlow;

use sqlparser::ast::{
    visit_expressions, visit_relations, BinaryOperator, Expr, SetExpr, Statement, Value,
};
use sqlparser::dialect::PostgreSqlDialect;
use sqlparser::parser::Parser;

use crate::error::CacheError;

pub enum Plan {
    Cacheable {
        fingerprint: u64,
        tables: Vec<String>,
        eq_filters: Vec<(String, String)>,
        sql: String,
    },
    PassThrough {
        sql: String,
    },
    Forward {
        sql: String,
    },
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

    pub fn classify(&self, sql: &str) -> Result<Plan, CacheError> {
        let statements = match Parser::parse_sql(&PostgreSqlDialect {}, sql) {
            Ok(statements) => statements,
            Err(_) => {
                return Ok(Plan::Forward {
                    sql: sql.to_string(),
                })
            }
        };

        if statements.len() != 1 {
            return Err(CacheError::Rejected(
                "exactly one statement is supported".to_string(),
            ));
        }

        let statement = &statements[0];
        let query = match statement {
            Statement::Query(query) => query,
            _ => {
                return Ok(Plan::Forward {
                    sql: sql.to_string(),
                })
            }
        };

        if !query.locks.is_empty() {
            return Ok(Plan::Forward {
                sql: sql.to_string(),
            });
        }
        if let SetExpr::Select(select) = query.body.as_ref() {
            if select.into.is_some() {
                return Ok(Plan::Forward {
                    sql: sql.to_string(),
                });
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
        if functions
            .iter()
            .any(|f| SIDE_EFFECTING.contains(&f.as_str()))
        {
            return Ok(Plan::Forward {
                sql: sql.to_string(),
            });
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
                    "table `{table}` is not replicated"
                )));
            }
        }

        if functions
            .iter()
            .any(|f| VOLATILE_READ.contains(&f.as_str()))
        {
            return Ok(Plan::PassThrough {
                sql: sql.to_string(),
            });
        }

        let mut eq_filters = Vec::new();
        if let SetExpr::Select(select) = query.body.as_ref() {
            if let Some(selection) = &select.selection {
                collect_eq_filters(selection, &mut eq_filters);
            }
        }

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        statement.to_string().hash(&mut hasher);

        Ok(Plan::Cacheable {
            fingerprint: hasher.finish(),
            tables,
            eq_filters,
            sql: sql.to_string(),
        })
    }
}

fn collect_eq_filters(expr: &Expr, out: &mut Vec<(String, String)>) {
    match expr {
        Expr::BinaryOp {
            left,
            op: BinaryOperator::And,
            right,
        } => {
            collect_eq_filters(left, out);
            collect_eq_filters(right, out);
        }
        Expr::BinaryOp {
            left,
            op: BinaryOperator::Eq,
            right,
        } => {
            if let (Some(column), Some(value)) = (ident_name(left), literal_value(right)) {
                out.push((column, value));
            } else if let (Some(column), Some(value)) = (ident_name(right), literal_value(left)) {
                out.push((column, value));
            }
        }
        Expr::Nested(inner) => collect_eq_filters(inner, out),
        _ => {}
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
