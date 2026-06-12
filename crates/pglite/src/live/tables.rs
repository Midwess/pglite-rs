use super::LiveQuery;

pub(crate) const WATCHED_TABLES_SQL: &str = "
WITH RECURSIVE view_dependencies AS (
  SELECT DISTINCT
    cl.relname AS dependent_name,
    n.nspname AS schema_name,
    cl.oid AS dependent_oid,
    n.oid AS schema_oid,
    cl.relkind = 'v' AS is_view
  FROM pg_rewrite r
  JOIN pg_depend d ON r.oid = d.objid
  JOIN pg_class cl ON d.refobjid = cl.oid
  JOIN pg_namespace n ON cl.relnamespace = n.oid
  WHERE
    r.ev_class = (
        SELECT oid FROM pg_class WHERE relname = $1 AND relkind = 'v'
    )
    AND d.deptype = 'n'

  UNION ALL

  SELECT DISTINCT
    cl.relname AS dependent_name,
    n.nspname AS schema_name,
    cl.oid AS dependent_oid,
    n.oid AS schema_oid,
    cl.relkind = 'v' AS is_view
  FROM view_dependencies vd
  JOIN pg_rewrite r ON vd.dependent_name = (
    SELECT relname FROM pg_class WHERE oid = r.ev_class AND relkind = 'v'
  )
  JOIN pg_depend d ON r.oid = d.objid
  JOIN pg_class cl ON d.refobjid = cl.oid
  JOIN pg_namespace n ON cl.relnamespace = n.oid
  WHERE d.deptype = 'n'
)
SELECT DISTINCT
  dependent_name AS table_name,
  schema_name,
  dependent_oid::int4 AS table_oid,
  schema_oid::int4
FROM view_dependencies
WHERE NOT is_view
";

impl LiveQuery {
    pub(crate) fn channel_name(schema_oid: u32, table_oid: u32) -> String {
        format!("table_change__{schema_oid}__{table_oid}")
    }

    pub(crate) fn trigger_ddl(
        schema_oid: u32,
        table_oid: u32,
        schema_name: &str,
        table_name: &str,
    ) -> String {
        format!(
            "CREATE OR REPLACE FUNCTION \"_notify_{schema_oid}_{table_oid}\"() RETURNS TRIGGER AS $$
BEGIN
  PERFORM pg_notify('table_change__{schema_oid}__{table_oid}', '');
  RETURN NULL;
END;
$$ LANGUAGE plpgsql;
CREATE OR REPLACE TRIGGER \"_notify_trigger_{schema_oid}_{table_oid}\"
AFTER INSERT OR UPDATE OR DELETE ON \"{schema_name}\".\"{table_name}\"
FOR EACH STATEMENT EXECUTE FUNCTION \"_notify_{schema_oid}_{table_oid}\"();"
        )
    }
}
