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
    pub replica_identity: String,
    pub row_filter: Option<String>,
    pub indexes: Vec<String>,
}

#[derive(Debug)]
pub(crate) enum TypeDef {
    Enum {
        schema: String,
        name: String,
        labels: Vec<String>,
    },
    Composite {
        schema: String,
        name: String,
        attrs: Vec<(String, String)>,
    },
    Domain {
        schema: String,
        name: String,
        base: String,
        not_null: bool,
        checks: Vec<String>,
    },
    Range {
        schema: String,
        name: String,
        subtype: String,
    },
}

impl TypeDef {
    fn schema(&self) -> &str {
        match self {
            TypeDef::Enum { schema, .. }
            | TypeDef::Composite { schema, .. }
            | TypeDef::Domain { schema, .. }
            | TypeDef::Range { schema, .. } => schema,
        }
    }

    fn name(&self) -> &str {
        match self {
            TypeDef::Enum { name, .. }
            | TypeDef::Composite { name, .. }
            | TypeDef::Domain { name, .. }
            | TypeDef::Range { name, .. } => name,
        }
    }

    fn target(&self) -> String {
        format!("{}.{}", ident(self.schema()), ident(self.name()))
    }

    fn drop_sql(&self) -> String {
        let kind = match self {
            TypeDef::Domain { .. } => "DOMAIN",
            _ => "TYPE",
        };
        format!("DROP {kind} IF EXISTS {} CASCADE", self.target())
    }

    fn create_sql(&self) -> String {
        let target = self.target();
        match self {
            TypeDef::Enum { labels, .. } => {
                let labels = labels.iter().map(|l| lit(l)).collect::<Vec<_>>().join(", ");
                format!("CREATE TYPE {target} AS ENUM ({labels})")
            }
            TypeDef::Composite { attrs, .. } => {
                let attrs = attrs
                    .iter()
                    .map(|(n, ty)| format!("{} {}", ident(n), ty))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("CREATE TYPE {target} AS ({attrs})")
            }
            TypeDef::Domain {
                base,
                not_null,
                checks,
                ..
            } => {
                let mut sql = format!("CREATE DOMAIN {target} AS {base}");
                if *not_null {
                    sql.push_str(" NOT NULL");
                }
                for check in checks {
                    sql.push(' ');
                    sql.push_str(check);
                }
                sql
            }
            TypeDef::Range { subtype, .. } => {
                format!("CREATE TYPE {target} AS RANGE (subtype = {subtype})")
            }
        }
    }
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
            let relreplident = get(6)?;
            check_replica_identity(schema, table, relreplident)?;
            tables.push(TableDef {
                schema: schema.to_string(),
                name: table.to_string(),
                columns: Vec::new(),
                pk: Vec::new(),
                replica_identity: relreplident.to_string(),
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
        if t.pk.is_empty() && t.replica_identity != "f" {
            return Err(Error::ReplicaConfig(format!(
                "published table {schema}.{table} has no primary key and is not REPLICA IDENTITY FULL; \
                 run ALTER TABLE {schema}.{table} REPLICA IDENTITY FULL so its updates and deletes can be replicated",
                schema = t.schema,
                table = t.name
            )));
        }
    }
    Ok(tables)
}

fn oid_array(oids: impl Iterator<Item = u32>) -> String {
    format!(
        "ARRAY[{}]::oid[]",
        oids.map(|o| o.to_string()).collect::<Vec<_>>().join(", ")
    )
}

pub(crate) fn check_extension_types(snap: &mut ReplConn, tables: &[TableDef]) -> Result<(), Error> {
    let mut oids: Vec<u32> = Vec::new();
    for t in tables {
        for c in &t.columns {
            if !oids.contains(&c.type_oid) {
                oids.push(c.type_oid);
            }
        }
    }
    if oids.is_empty() {
        return Ok(());
    }
    let rows = snap.simple_query(&format!(
        "SELECT a.col_oid::text, n.nspname::text, t.typname::text, e.extname::text \
         FROM (SELECT unnest({}) AS col_oid) a \
         JOIN pg_type base ON base.oid = a.col_oid \
         JOIN pg_type t ON t.oid = CASE WHEN base.typtype = 'b' AND base.typelem <> 0 \
                                        THEN base.typelem ELSE base.oid END \
         JOIN pg_depend d ON d.objid = t.oid AND d.classid = 'pg_type'::regclass AND d.deptype = 'e' \
         JOIN pg_extension e ON e.oid = d.refobjid \
         JOIN pg_namespace n ON n.oid = t.typnamespace",
        oid_array(oids.iter().copied())
    ))?;
    let Some(row) = rows.first() else {
        return Ok(());
    };
    let cell = |i: usize| row.get(i).and_then(|v| v.as_deref()).unwrap_or_default();
    let col_oid: u32 = cell(0).parse().unwrap_or(0);
    let location = tables
        .iter()
        .flat_map(|t| t.columns.iter().map(move |c| (t, c)))
        .find(|(_, c)| c.type_oid == col_oid)
        .map(|(t, c)| format!("{}.{}.{}", t.schema, t.name, c.name))
        .unwrap_or_else(|| "a published column".to_string());
    Err(Error::ReplicaConfig(format!(
        "column {location} uses type {}.{} from extension '{}', which pglite does not provide; \
         exclude the table from the publication or remove the column",
        cell(1),
        cell(2),
        cell(3)
    )))
}

pub(crate) fn introspect_types(
    snap: &mut ReplConn,
    publication: &str,
) -> Result<Vec<TypeDef>, Error> {
    let pub_lit = lit(publication);
    let rows = snap.simple_query(&format!(
        "WITH RECURSIVE used(oid) AS ( \
           SELECT a.atttypid \
           FROM pg_publication_tables pt \
           JOIN pg_namespace tn ON tn.nspname = pt.schemaname \
           JOIN pg_class c ON c.relnamespace = tn.oid AND c.relname = pt.tablename \
           JOIN pg_attribute a ON a.attrelid = c.oid AND a.attnum > 0 AND NOT a.attisdropped \
           WHERE pt.pubname = {pub_lit} \
         UNION \
           SELECT dep.oid \
           FROM used u \
           JOIN pg_type t ON t.oid = u.oid \
           CROSS JOIN LATERAL ( \
             SELECT t.typelem AS oid WHERE t.typelem <> 0 \
             UNION ALL SELECT t.typbasetype WHERE t.typtype = 'd' \
             UNION ALL SELECT rng.rngsubtype FROM pg_range rng WHERE rng.rngtypid = t.oid \
             UNION ALL SELECT ca.atttypid FROM pg_attribute ca \
               WHERE t.typtype = 'c' AND ca.attrelid = t.typrelid \
                 AND ca.attnum > 0 AND NOT ca.attisdropped \
           ) dep \
           WHERE dep.oid <> 0 \
         ) \
         SELECT u.oid::text, t.typtype::text, n.nspname::text, t.typname::text, \
                t.typbasetype::text, t.typnotnull::int, format_type(t.typbasetype, t.typtypmod), \
                (SELECT rng.rngsubtype::text FROM pg_range rng WHERE rng.rngtypid = t.oid), \
                format_type((SELECT rng.rngsubtype FROM pg_range rng WHERE rng.rngtypid = t.oid), -1) \
         FROM used u \
         JOIN pg_type t ON t.oid = u.oid \
         JOIN pg_namespace n ON n.oid = t.typnamespace \
         WHERE t.typtype IN ('e', 'c', 'd', 'r')"
    ))?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }

    struct Raw {
        oid: u32,
        typtype: String,
        schema: String,
        name: String,
        basetype: u32,
        not_null: bool,
        base_fmt: String,
        rngsub: u32,
        rngsub_fmt: String,
    }
    let parse_oid = |v: Option<&str>| -> u32 { v.and_then(|s| s.parse().ok()).unwrap_or(0) };
    let mut raws: Vec<Raw> = Vec::with_capacity(rows.len());
    let mut oidset: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for row in &rows {
        let cell = |i: usize| row.get(i).and_then(|v| v.as_deref());
        let oid = parse_oid(cell(0));
        oidset.insert(oid);
        raws.push(Raw {
            oid,
            typtype: cell(1).unwrap_or_default().to_string(),
            schema: cell(2).unwrap_or_default().to_string(),
            name: cell(3).unwrap_or_default().to_string(),
            basetype: parse_oid(cell(4)),
            not_null: cell(5) == Some("1"),
            base_fmt: cell(6).unwrap_or_default().to_string(),
            rngsub: parse_oid(cell(7)),
            rngsub_fmt: cell(8).unwrap_or_default().to_string(),
        });
    }

    let oids_of = |kind: &str| -> Vec<u32> {
        raws.iter()
            .filter(|r| r.typtype == kind)
            .map(|r| r.oid)
            .collect()
    };
    let comp_oids = oids_of("c");
    let enum_oids = oids_of("e");
    let dom_oids = oids_of("d");

    let mut attrs: std::collections::HashMap<u32, Vec<(String, String)>> = Default::default();
    let mut comp_deps: std::collections::HashMap<u32, Vec<u32>> = Default::default();
    if !comp_oids.is_empty() {
        let rows = snap.simple_query(&format!(
            "SELECT t.oid::text, a.attname::text, format_type(a.atttypid, a.atttypmod), a.atttypid::text \
             FROM pg_type t JOIN pg_attribute a ON a.attrelid = t.typrelid \
             WHERE t.oid = ANY({}) AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY t.oid, a.attnum",
            oid_array(comp_oids.iter().copied())
        ))?;
        for row in &rows {
            let cell = |i: usize| row.get(i).and_then(|v| v.as_deref());
            let oid = parse_oid(cell(0));
            attrs.entry(oid).or_default().push((
                cell(1).unwrap_or_default().to_string(),
                cell(2).unwrap_or_default().to_string(),
            ));
            comp_deps.entry(oid).or_default().push(parse_oid(cell(3)));
        }
    }

    let mut labels: std::collections::HashMap<u32, Vec<String>> = Default::default();
    if !enum_oids.is_empty() {
        let rows = snap.simple_query(&format!(
            "SELECT t.oid::text, e.enumlabel::text \
             FROM pg_enum e JOIN pg_type t ON t.oid = e.enumtypid \
             WHERE t.oid = ANY({}) ORDER BY t.oid, e.enumsortorder",
            oid_array(enum_oids.iter().copied())
        ))?;
        for row in &rows {
            let oid = parse_oid(row.first().and_then(|v| v.as_deref()));
            labels.entry(oid).or_default().push(
                row.get(1)
                    .and_then(|v| v.as_deref())
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }

    let mut checks: std::collections::HashMap<u32, Vec<String>> = Default::default();
    if !dom_oids.is_empty() {
        let rows = snap.simple_query(&format!(
            "SELECT con.contypid::text, pg_get_constraintdef(con.oid) \
             FROM pg_constraint con \
             WHERE con.contypid = ANY({}) AND con.contype = 'c' \
             ORDER BY con.contypid, con.conname",
            oid_array(dom_oids.iter().copied())
        ))?;
        for row in &rows {
            let oid = parse_oid(row.first().and_then(|v| v.as_deref()));
            checks.entry(oid).or_default().push(
                row.get(1)
                    .and_then(|v| v.as_deref())
                    .unwrap_or_default()
                    .to_string(),
            );
        }
    }

    let mut items: Vec<(u32, Vec<u32>, TypeDef)> = Vec::with_capacity(raws.len());
    for r in raws {
        let in_set = |o: u32| o != 0 && oidset.contains(&o);
        let (def, deps) = match r.typtype.as_str() {
            "e" => (
                TypeDef::Enum {
                    schema: r.schema,
                    name: r.name,
                    labels: labels.remove(&r.oid).unwrap_or_default(),
                },
                Vec::new(),
            ),
            "c" => {
                let deps = comp_deps
                    .remove(&r.oid)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|o| in_set(*o))
                    .collect();
                (
                    TypeDef::Composite {
                        schema: r.schema,
                        name: r.name,
                        attrs: attrs.remove(&r.oid).unwrap_or_default(),
                    },
                    deps,
                )
            }
            "d" => {
                let deps = if in_set(r.basetype) {
                    vec![r.basetype]
                } else {
                    Vec::new()
                };
                (
                    TypeDef::Domain {
                        schema: r.schema,
                        name: r.name,
                        base: r.base_fmt,
                        not_null: r.not_null,
                        checks: checks.remove(&r.oid).unwrap_or_default(),
                    },
                    deps,
                )
            }
            "r" => {
                let deps = if in_set(r.rngsub) {
                    vec![r.rngsub]
                } else {
                    Vec::new()
                };
                (
                    TypeDef::Range {
                        schema: r.schema,
                        name: r.name,
                        subtype: r.rngsub_fmt,
                    },
                    deps,
                )
            }
            _ => continue,
        };
        items.push((r.oid, deps, def));
    }

    topo_order(items)
}

fn topo_order(items: Vec<(u32, Vec<u32>, TypeDef)>) -> Result<Vec<TypeDef>, Error> {
    let index: std::collections::HashMap<u32, usize> =
        items.iter().enumerate().map(|(i, it)| (it.0, i)).collect();
    let n = items.len();
    let mut state = vec![0u8; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for start in 0..n {
        if state[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0usize)];
        while let Some(&(node, di)) = stack.last() {
            if di == 0 {
                state[node] = 1;
            }
            let deps = &items[node].1;
            let mut k = di;
            let mut next = None;
            while k < deps.len() {
                let dep = deps[k];
                k += 1;
                if let Some(&j) = index.get(&dep) {
                    match state[j] {
                        0 => {
                            next = Some(j);
                            break;
                        }
                        1 => {
                            return Err(Error::ReplicaConfig(format!(
                                "cyclic user-defined type dependency involving type oid {}",
                                items[node].0
                            )))
                        }
                        _ => {}
                    }
                }
            }
            stack.last_mut().unwrap().1 = k;
            match next {
                Some(j) => stack.push((j, 0)),
                None => {
                    state[node] = 2;
                    order.push(node);
                    stack.pop();
                }
            }
        }
    }
    let mut defs: Vec<Option<TypeDef>> = items.into_iter().map(|it| Some(it.2)).collect();
    Ok(order.into_iter().map(|i| defs[i].take().unwrap()).collect())
}

#[derive(Debug, PartialEq)]
pub(crate) enum SchemaChange {
    Additive(usize),
    Incompatible(String),
}

pub(crate) fn classify_schema_change(expected_line: &str, new: &[(&str, u32)]) -> SchemaChange {
    let old: Vec<(&str, u32)> = expected_line
        .split('|')
        .skip(2)
        .filter_map(|seg| {
            let (name, oid) = seg.rsplit_once(':')?;
            Some((name, oid.parse().ok()?))
        })
        .collect();
    if new.len() < old.len() {
        return SchemaChange::Incompatible(format!(
            "a column was removed (had {}, now {})",
            old.len(),
            new.len()
        ));
    }
    for (i, (old_name, old_oid)) in old.iter().enumerate() {
        let (new_name, new_oid) = new[i];
        if old_name != &new_name || old_oid != &new_oid {
            return SchemaChange::Incompatible(format!(
                "column {i} changed (was {old_name}:{old_oid}, now {new_name}:{new_oid})"
            ));
        }
    }
    SchemaChange::Additive(old.len())
}

fn create_table_sql(t: &TableDef) -> String {
    let target = format!("{}.{}", ident(&t.schema), ident(&t.name));
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
    if !t.pk.is_empty() {
        defs.push(format!(
            "PRIMARY KEY ({})",
            t.pk.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
        ));
    }
    format!("CREATE TABLE {target} ({})", defs.join(", "))
}

pub(crate) fn bootstrap_schema(
    db: &PGlite,
    types: &[TypeDef],
    tables: &[TableDef],
) -> Result<(), Error> {
    block_on(async {
        for ty in types {
            if ty.schema() != "public" {
                db.exec(&format!(
                    "CREATE SCHEMA IF NOT EXISTS {}",
                    ident(ty.schema())
                ))
                .await?;
            }
            db.exec(&ty.drop_sql()).await?;
            db.exec(&ty.create_sql()).await?;
        }
        for t in tables {
            if t.schema != "public" {
                db.exec(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&t.schema)))
                    .await?;
            }
            let target = format!("{}.{}", ident(&t.schema), ident(&t.name));
            db.exec(&format!("DROP TABLE IF EXISTS {target} CASCADE"))
                .await?;
            db.exec(&create_table_sql(t)).await?;
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

pub(crate) fn apply_schema_delta(
    snap: &mut ReplConn,
    db: &PGlite,
    publication: &str,
    old_lines: &[String],
) -> Result<Vec<TableDef>, Error> {
    let new_tables = introspect(snap, publication)?;
    check_extension_types(snap, &new_tables)?;
    let new_types = introspect_types(snap, publication)?;

    let old: std::collections::HashMap<(String, String), Vec<(String, u32)>> = old_lines
        .iter()
        .filter_map(|line| {
            let mut parts = line.split('|');
            let schema = parts.next()?.to_string();
            let table = parts.next()?.to_string();
            let cols = parts
                .filter_map(|seg| {
                    let (name, oid) = seg.rsplit_once(':')?;
                    Some((name.to_string(), oid.parse().ok()?))
                })
                .collect();
            Some(((schema, table), cols))
        })
        .collect();

    let new_keys: std::collections::HashSet<(String, String)> = new_tables
        .iter()
        .map(|t| (t.schema.clone(), t.name.clone()))
        .collect();

    block_on(async {
        for ty in &new_types {
            let schema = ty.schema();
            let name = ty.name();
            let exists = !db
                .query(
                    "SELECT 1 FROM pg_type t JOIN pg_namespace n ON n.oid = t.typnamespace \
                     WHERE n.nspname = $1 AND t.typname = $2",
                    &[&schema, &name],
                )
                .await?
                .is_empty();
            if !exists {
                if schema != "public" {
                    db.exec(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(schema)))
                        .await?;
                }
                db.exec(&ty.create_sql()).await?;
            }
        }

        for (schema, table) in old.keys() {
            if !new_keys.contains(&(schema.clone(), table.clone())) {
                db.exec(&format!(
                    "DROP TABLE IF EXISTS {}.{} CASCADE",
                    ident(schema),
                    ident(table)
                ))
                .await?;
            }
        }

        for t in &new_tables {
            let key = (t.schema.clone(), t.name.clone());
            match old.get(&key) {
                None => {
                    if t.schema != "public" {
                        db.exec(&format!("CREATE SCHEMA IF NOT EXISTS {}", ident(&t.schema)))
                            .await?;
                    }
                    db.exec(&format!(
                        "DROP TABLE IF EXISTS {}.{} CASCADE",
                        ident(&t.schema),
                        ident(&t.name)
                    ))
                    .await?;
                    db.exec(&create_table_sql(t)).await?;
                    for index in &t.indexes {
                        db.query(index, &[]).await?;
                    }
                }
                Some(old_cols) => {
                    let target = format!("{}.{}", ident(&t.schema), ident(&t.name));
                    let old_names: std::collections::HashSet<&str> =
                        old_cols.iter().map(|(n, _)| n.as_str()).collect();
                    let new_names: std::collections::HashSet<&str> =
                        t.columns.iter().map(|c| c.name.as_str()).collect();
                    for c in &t.columns {
                        if !old_names.contains(c.name.as_str()) {
                            db.exec(&format!(
                                "ALTER TABLE {target} ADD COLUMN IF NOT EXISTS {} {}",
                                ident(&c.name),
                                c.type_sql
                            ))
                            .await?;
                        }
                    }
                    for (name, old_oid) in old_cols {
                        if !new_names.contains(name.as_str()) {
                            db.exec(&format!(
                                "ALTER TABLE {target} DROP COLUMN IF EXISTS {}",
                                ident(name)
                            ))
                            .await?;
                        } else if t
                            .columns
                            .iter()
                            .any(|c| &c.name == name && c.type_oid != *old_oid)
                        {
                            return Err(Error::ReplicaHalted(format!(
                                "incompatible column type change on {}.{} column {name}; \
                                 run Replica::decommission and start again for a full resync",
                                t.schema, t.name
                            )));
                        }
                    }
                }
            }
        }
        Ok::<(), Error>(())
    })?;

    Ok(new_tables)
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

    fn tdef(pk: Vec<&str>, replica_identity: &str) -> TableDef {
        TableDef {
            schema: "public".into(),
            name: "t".into(),
            columns: vec![ColDef {
                name: "id".into(),
                type_sql: "integer".into(),
                type_oid: 23,
                not_null: true,
            }],
            pk: pk.into_iter().map(String::from).collect(),
            replica_identity: replica_identity.into(),
            row_filter: None,
            indexes: Vec::new(),
        }
    }

    #[test]
    fn classify_schema_change_additive() {
        assert_eq!(
            classify_schema_change("public|t|id:23", &[("id", 23), ("extra", 25)]),
            SchemaChange::Additive(1)
        );
        assert_eq!(
            classify_schema_change("public|t|id:23|name:25", &[("id", 23), ("name", 25)]),
            SchemaChange::Additive(2)
        );
    }

    #[test]
    fn classify_schema_change_incompatible() {
        assert!(matches!(
            classify_schema_change("public|t|id:23|x:25", &[("id", 23)]),
            SchemaChange::Incompatible(_)
        ));
        assert!(matches!(
            classify_schema_change("public|t|id:23", &[("id", 1700)]),
            SchemaChange::Incompatible(_)
        ));
        assert!(matches!(
            classify_schema_change("public|t|a:23|b:25", &[("b", 25), ("a", 23)]),
            SchemaChange::Incompatible(_)
        ));
    }

    #[test]
    fn type_def_create_sql_per_kind() {
        assert_eq!(
            TypeDef::Enum {
                schema: "public".into(),
                name: "mood".into(),
                labels: vec!["sad".into(), "ok".into()],
            }
            .create_sql(),
            "CREATE TYPE \"public\".\"mood\" AS ENUM ('sad', 'ok')"
        );
        assert_eq!(
            TypeDef::Composite {
                schema: "public".into(),
                name: "addr".into(),
                attrs: vec![
                    ("street".into(), "text".into()),
                    ("zip".into(), "integer".into()),
                ],
            }
            .create_sql(),
            "CREATE TYPE \"public\".\"addr\" AS (\"street\" text, \"zip\" integer)"
        );
        assert_eq!(
            TypeDef::Domain {
                schema: "public".into(),
                name: "pos".into(),
                base: "integer".into(),
                not_null: true,
                checks: vec!["CHECK ((VALUE > 0))".into()],
            }
            .create_sql(),
            "CREATE DOMAIN \"public\".\"pos\" AS integer NOT NULL CHECK ((VALUE > 0))"
        );
        assert_eq!(
            TypeDef::Range {
                schema: "public".into(),
                name: "floatrange".into(),
                subtype: "double precision".into(),
            }
            .create_sql(),
            "CREATE TYPE \"public\".\"floatrange\" AS RANGE (subtype = double precision)"
        );
    }

    #[test]
    fn type_def_drop_sql_uses_domain_keyword() {
        let dom = TypeDef::Domain {
            schema: "public".into(),
            name: "pos".into(),
            base: "integer".into(),
            not_null: false,
            checks: Vec::new(),
        };
        assert_eq!(
            dom.drop_sql(),
            "DROP DOMAIN IF EXISTS \"public\".\"pos\" CASCADE"
        );
        let en = TypeDef::Enum {
            schema: "public".into(),
            name: "mood".into(),
            labels: Vec::new(),
        };
        assert_eq!(
            en.drop_sql(),
            "DROP TYPE IF EXISTS \"public\".\"mood\" CASCADE"
        );
    }

    fn named(oid: u32) -> TypeDef {
        TypeDef::Enum {
            schema: "public".into(),
            name: format!("t{oid}"),
            labels: Vec::new(),
        }
    }

    #[test]
    fn topo_order_emits_dependencies_first() {
        // 3 depends on 2 depends on 1
        let items = vec![
            (3u32, vec![2u32], named(3)),
            (1u32, vec![], named(1)),
            (2u32, vec![1u32], named(2)),
        ];
        let order = topo_order(items).unwrap();
        let names: Vec<&str> = order
            .iter()
            .map(|t| match t {
                TypeDef::Enum { name, .. } => name.as_str(),
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(names, vec!["t1", "t2", "t3"]);
    }

    #[test]
    fn topo_order_detects_cycle() {
        let items = vec![(1u32, vec![2u32], named(1)), (2u32, vec![1u32], named(2))];
        let err = topo_order(items).unwrap_err().to_string();
        assert!(err.contains("cyclic"), "{err}");
    }

    #[test]
    fn create_table_sql_includes_pk() {
        let sql = create_table_sql(&tdef(vec!["id"], "d"));
        assert_eq!(
            sql,
            "CREATE TABLE \"public\".\"t\" (\"id\" integer NOT NULL, PRIMARY KEY (\"id\"))"
        );
    }

    #[test]
    fn create_table_sql_omits_pk_for_full_identity_no_pk() {
        let sql = create_table_sql(&tdef(vec![], "f"));
        assert_eq!(
            sql,
            "CREATE TABLE \"public\".\"t\" (\"id\" integer NOT NULL)"
        );
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
