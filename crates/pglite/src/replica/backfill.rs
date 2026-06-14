use futures::executor::block_on;

use super::wire::ReplConn;
use super::{ident, lit};
use crate::db::PGlite;
use crate::error::Error;

const COPY_BATCH_BYTES: usize = 1 << 20;

pub(crate) struct ColDef {
    pub name: String,
    pub type_sql: String,
    pub type_oid: u32,
    pub not_null: bool,
}

pub(crate) struct TableDef {
    pub schema: String,
    pub name: String,
    pub columns: Vec<ColDef>,
    pub pk: Vec<String>,
    pub row_filter: Option<String>,
    pub indexes: Vec<String>,
}

pub(crate) struct EnumDef {
    pub schema: String,
    pub name: String,
    pub labels: Vec<String>,
}

pub(crate) fn fingerprint_line<'a>(
    schema: &str,
    table: &str,
    columns: impl Iterator<Item = (&'a str, u32)>,
) -> String {
    let mut line = format!("{schema}|{table}");
    for (name, oid) in columns {
        line.push('|');
        line.push_str(name);
        line.push(':');
        line.push_str(&oid.to_string());
    }
    line
}

pub(crate) fn fingerprint(tables: &[TableDef]) -> String {
    tables
        .iter()
        .map(|t| {
            fingerprint_line(
                &t.schema,
                &t.name,
                t.columns.iter().map(|c| (c.name.as_str(), c.type_oid)),
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn published_columns_filter(server_version_num: u32) -> &'static str {
    if server_version_num >= 150000 {
        "AND a.attgenerated = '' AND a.attname = ANY(pt.attnames)"
    } else {
        "AND a.attgenerated = ''"
    }
}

fn validate_publish_ops(publication: &str, flags: [bool; 4]) -> Result<(), Error> {
    for (published, op) in flags.iter().zip(["insert", "update", "delete", "truncate"]) {
        if !*published {
            return Err(Error::ReplicaConfig(format!(
                "publication {publication} does not publish {op}; the replica cache would silently diverge"
            )));
        }
    }
    Ok(())
}

fn check_replica_identity(schema: &str, table: &str, relreplident: &str) -> Result<(), Error> {
    if relreplident == "n" {
        return Err(Error::ReplicaConfig(format!(
            "published table {schema}.{table} has REPLICA IDENTITY NOTHING; its updates and deletes cannot be replicated"
        )));
    }
    Ok(())
}

pub(crate) fn introspect(snap: &mut ReplConn, publication: &str) -> Result<Vec<TableDef>, Error> {
    let pub_lit = lit(publication);

    let ops = snap.simple_query(&format!(
        "SELECT pubinsert::int, pubupdate::int, pubdelete::int, pubtruncate::int \
         FROM pg_publication WHERE pubname = {pub_lit}"
    ))?;
    let ops_row = ops
        .first()
        .ok_or_else(|| Error::ReplicaConfig(format!("publication {publication} does not exist")))?;
    let published = |i: usize| ops_row.get(i).and_then(|v| v.as_deref()) == Some("1");
    validate_publish_ops(
        publication,
        [published(0), published(1), published(2), published(3)],
    )?;

    let version = snap.server_version_num()?;
    let col_filter = published_columns_filter(version);
    let rowfilter_col = if version >= 150000 {
        "pt.rowfilter"
    } else {
        "NULL::text"
    };
    let cols = snap.simple_query(&format!(
        "SELECT pt.schemaname::text, pt.tablename::text, a.attname::text, \
                format_type(a.atttypid, a.atttypmod), a.attnotnull::int, a.atttypid::text, \
                c.relreplident::text, {rowfilter_col} \
         FROM pg_publication_tables pt \
         JOIN pg_namespace n ON n.nspname = pt.schemaname \
         JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = pt.tablename \
         JOIN pg_attribute a ON a.attrelid = c.oid \
         WHERE pt.pubname = {pub_lit} AND a.attnum > 0 AND NOT a.attisdropped {col_filter} \
         ORDER BY pt.schemaname, pt.tablename, a.attnum"
    ))?;
    if cols.is_empty() {
        return Err(Error::ReplicaConfig(format!(
            "publication {publication} does not exist or contains no tables"
        )));
    }

    let mut tables: Vec<TableDef> = Vec::new();
    for row in &cols {
        let get = |i: usize| -> Result<&str, Error> {
            row.get(i)
                .and_then(|v| v.as_deref())
                .ok_or_else(|| Error::Protocol("null in introspection result".into()))
        };
        let schema = get(0)?;
        let table = get(1)?;
        if tables
            .last()
            .map(|t| t.schema != schema || t.name != table)
            .unwrap_or(true)
        {
            check_replica_identity(schema, table, get(6)?)?;
            tables.push(TableDef {
                schema: schema.to_string(),
                name: table.to_string(),
                columns: Vec::new(),
                pk: Vec::new(),
                row_filter: row.get(7).and_then(|v| v.as_deref()).map(str::to_string),
                indexes: Vec::new(),
            });
        }
        let type_oid: u32 = get(5)?
            .parse()
            .map_err(|_| Error::Protocol("bad type oid in introspection result".into()))?;
        tables.last_mut().unwrap().columns.push(ColDef {
            name: get(2)?.to_string(),
            type_sql: get(3)?.to_string(),
            type_oid,
            not_null: get(4)? == "1",
        });
    }

    let pks = snap.simple_query(&format!(
        "SELECT pt.schemaname::text, pt.tablename::text, a.attname::text \
         FROM pg_publication_tables pt \
         JOIN pg_namespace n ON n.nspname = pt.schemaname \
         JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = pt.tablename \
         JOIN pg_index i ON i.indrelid = c.oid AND i.indisprimary \
         JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum = ANY(i.indkey) \
         WHERE pt.pubname = {pub_lit} \
         ORDER BY pt.schemaname, pt.tablename, a.attnum"
    ))?;
    for row in &pks {
        let schema = row.first().and_then(|v| v.as_deref()).unwrap_or_default();
        let table = row.get(1).and_then(|v| v.as_deref()).unwrap_or_default();
        let col = row.get(2).and_then(|v| v.as_deref()).unwrap_or_default();
        if let Some(t) = tables
            .iter_mut()
            .find(|t| t.schema == schema && t.name == table)
        {
            t.pk.push(col.to_string());
        }
    }

    let unpublished_pred = if version >= 150000 {
        "a2.attname <> ALL(pt.attnames)"
    } else {
        "false"
    };
    let index_defs = snap.simple_query(&format!(
        "SELECT pt.schemaname::text, pt.tablename::text, pg_get_indexdef(i.indexrelid) \
         FROM pg_publication_tables pt \
         JOIN pg_namespace n ON n.nspname = pt.schemaname \
         JOIN pg_class c ON c.relnamespace = n.oid AND c.relname = pt.tablename \
         JOIN pg_index i ON i.indrelid = c.oid \
         WHERE pt.pubname = {pub_lit} \
           AND NOT i.indisprimary AND i.indisvalid AND i.indisready \
           AND i.indexprs IS NULL AND i.indpred IS NULL \
           AND NOT EXISTS ( \
             SELECT 1 FROM unnest(string_to_array(i.indkey::text, ' ')::int[]) AS col(attnum) \
             JOIN pg_attribute a2 ON a2.attrelid = c.oid AND a2.attnum = col.attnum \
             WHERE col.attnum > 0 AND {unpublished_pred} \
           ) \
         ORDER BY pt.schemaname, pt.tablename"
    ))?;
    for row in &index_defs {
        let schema = row.first().and_then(|v| v.as_deref()).unwrap_or_default();
        let table = row.get(1).and_then(|v| v.as_deref()).unwrap_or_default();
        let def = row.get(2).and_then(|v| v.as_deref()).unwrap_or_default();
        if let Some(t) = tables
            .iter_mut()
            .find(|t| t.schema == schema && t.name == table)
        {
            t.indexes.push(def.to_string());
        }
    }

    for t in &tables {
        if t.pk.is_empty() {
            return Err(Error::ReplicaConfig(format!(
                "published table {}.{} has no primary key",
                t.schema, t.name
            )));
        }
    }
    Ok(tables)
}

pub(crate) fn introspect_enums(
    snap: &mut ReplConn,
    publication: &str,
) -> Result<Vec<EnumDef>, Error> {
    let pub_lit = lit(publication);
    let rows = snap.simple_query(&format!(
        "WITH used AS ( \
           SELECT DISTINCT CASE WHEN col.typtype = 'e' THEN col.oid \
                                WHEN col.typelem <> 0 THEN col.typelem \
                                ELSE col.oid END AS type_oid \
           FROM pg_publication_tables pt \
           JOIN pg_namespace tn ON tn.nspname = pt.schemaname \
           JOIN pg_class c ON c.relnamespace = tn.oid AND c.relname = pt.tablename \
           JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
           JOIN pg_type col ON col.oid = a.atttypid \
           WHERE pt.pubname = {pub_lit} \
         ) \
         SELECT n.nspname::text, t.typname::text, e.enumlabel::text \
         FROM used u \
         JOIN pg_type t ON t.oid = u.type_oid AND t.typtype = 'e' \
         JOIN pg_enum e ON e.enumtypid = t.oid \
         JOIN pg_namespace n ON n.oid = t.typnamespace \
         ORDER BY n.nspname, t.typname, e.enumsortorder"
    ))?;

    let mut enums: Vec<EnumDef> = Vec::new();
    for row in &rows {
        let schema = row.first().and_then(|v| v.as_deref()).unwrap_or_default();
        let name = row.get(1).and_then(|v| v.as_deref()).unwrap_or_default();
        let label = row.get(2).and_then(|v| v.as_deref()).unwrap_or_default();
        if enums
            .last()
            .map(|e| e.schema != schema || e.name != name)
            .unwrap_or(true)
        {
            enums.push(EnumDef {
                schema: schema.to_string(),
                name: name.to_string(),
                labels: Vec::new(),
            });
        }
        enums.last_mut().unwrap().labels.push(label.to_string());
    }
    Ok(enums)
}

pub(crate) fn bootstrap_schema(
    db: &PGlite,
    enums: &[EnumDef],
    tables: &[TableDef],
) -> Result<(), Error> {
    block_on(async {
        for e in enums {
            if e.schema != "public" {
                db.exec(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&e.schema)))
                    .await?;
            }
            let target = format!("{}.{}", ident(&e.schema), ident(&e.name));
            db.exec(&format!("DROP TYPE IF EXISTS {target} CASCADE"))
                .await?;
            let labels = e.labels.iter().map(|l| lit(l)).collect::<Vec<_>>().join(", ");
            db.exec(&format!("CREATE TYPE {target} AS ENUM ({labels})"))
                .await?;
        }
        for t in tables {
            if t.schema != "public" {
                db.exec(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&t.schema)))
                    .await?;
            }
            let target = format!("{}.{}", ident(&t.schema), ident(&t.name));
            db.exec(&format!("DROP TABLE IF EXISTS {target} CASCADE"))
                .await?;
            let mut defs: Vec<String> = t
                .columns
                .iter()
                .map(|c| {
                    let mut d = format!("{} {}", ident(&c.name), c.type_sql);
                    if c.not_null {
                        d.push_str(" NOT NULL");
                    }
                    d
                })
                .collect();
            defs.push(format!(
                "PRIMARY KEY ({})",
                t.pk.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
            ));
            db.exec(&format!("CREATE TABLE {target} ({})", defs.join(", ")))
                .await?;
            for index in &t.indexes {
                db.query(index, &[]).await?;
            }
        }
        Ok(())
    })
}

pub(crate) fn copy_tables(
    snap: &mut ReplConn,
    db: &PGlite,
    tables: &[TableDef],
) -> Result<(), Error> {
    for t in tables {
        let target = format!("{}.{}", ident(&t.schema), ident(&t.name));
        let col_list = t
            .columns
            .iter()
            .map(|c| ident(&c.name))
            .collect::<Vec<_>>()
            .join(", ");
        let copy_in_sql = format!("COPY {target} ({col_list}) FROM STDIN");
        let copy_out_sql = match &t.row_filter {
            Some(filter) => {
                format!("COPY (SELECT {col_list} FROM {target} WHERE {filter}) TO STDOUT")
            }
            None => format!("COPY {target} ({col_list}) TO STDOUT"),
        };
        let mut batch: Vec<u8> = Vec::with_capacity(COPY_BATCH_BYTES);
        snap.copy_out(&copy_out_sql, |chunk| {
            batch.extend_from_slice(chunk);
            if batch.len() >= COPY_BATCH_BYTES {
                block_on(db.copy_in(&copy_in_sql, &batch))?;
                batch.clear();
            }
            Ok(())
        })?;
        if !batch.is_empty() {
            block_on(db.copy_in(&copy_in_sql, &batch))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn column_filter_gates_attnames_on_pg15() {
        assert_eq!(published_columns_filter(140000), "AND a.attgenerated = ''");
        assert_eq!(
            published_columns_filter(150000),
            "AND a.attgenerated = '' AND a.attname = ANY(pt.attnames)"
        );
        assert_eq!(
            published_columns_filter(170004),
            "AND a.attgenerated = '' AND a.attname = ANY(pt.attnames)"
        );
    }

    #[test]
    fn publish_ops_requires_all_four() {
        assert!(validate_publish_ops("p", [true, true, true, true]).is_ok());
        for missing in 0..4 {
            let mut flags = [true; 4];
            flags[missing] = false;
            let err = validate_publish_ops("p", flags).unwrap_err().to_string();
            assert!(
                err.contains("does not publish") && err.contains("diverge"),
                "{err}"
            );
        }
        let err = validate_publish_ops("p", [true, false, true, true])
            .unwrap_err()
            .to_string();
        assert!(err.contains("update"), "{err}");
    }

    #[test]
    fn replica_identity_nothing_rejected() {
        for ok in ["d", "f", "i"] {
            assert!(check_replica_identity("public", "t", ok).is_ok());
        }
        let err = check_replica_identity("public", "t", "n")
            .unwrap_err()
            .to_string();
        assert!(err.contains("REPLICA IDENTITY NOTHING"), "{err}");
        assert!(err.contains("public.t"), "{err}");
    }
}
