use pglite::PGlite;

use crate::error::CacheError;

pub fn wrap_json(sql: &str) -> String {
    format!("select coalesce(jsonb_agg(to_jsonb(_t)), '[]'::jsonb)::text as j from ({sql}) _t")
}

pub async fn query_json(db: &PGlite, sql: &str) -> Result<String, CacheError> {
    let rows = db.query(&wrap_json(sql), &[]).await?;
    let body = match rows.first() {
        Some(row) => row.get::<Option<String>>(0)?,
        None => None,
    };
    Ok(body.unwrap_or_else(|| "[]".to_string()))
}
