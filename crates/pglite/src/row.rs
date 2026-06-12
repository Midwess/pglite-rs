use std::sync::Arc;

use fallible_iterator::FallibleIterator;
use postgres_protocol::message::backend::DataRowBody;
use postgres_types::{FromSql, Type};

use crate::error::Error;

pub struct Column {
    name: String,
    type_: Type,
}

impl Column {
    pub(crate) fn new(name: String, type_: Type) -> Column {
        Column { name, type_ }
    }

    pub(crate) fn type_from_oid(oid: u32) -> Type {
        Type::from_oid(oid).unwrap_or(Type::TEXT)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn type_(&self) -> &Type {
        &self.type_
    }
}

pub struct Row {
    columns: Arc<Vec<Column>>,
    body: DataRowBody,
    ranges: Vec<Option<std::ops::Range<usize>>>,
}

impl Row {
    pub(crate) fn new(columns: Arc<Vec<Column>>, body: DataRowBody) -> Result<Row, Error> {
        let ranges = body
            .ranges()
            .collect::<Vec<_>>()
            .map_err(|e| Error::Protocol(e.to_string()))?;
        Ok(Row {
            columns,
            body,
            ranges,
        })
    }

    pub fn columns(&self) -> &[Column] {
        &self.columns
    }

    pub fn len(&self) -> usize {
        self.columns.len()
    }

    pub fn is_empty(&self) -> bool {
        self.columns.is_empty()
    }

    pub fn get<'a, T: FromSql<'a>>(&'a self, idx: usize) -> Result<T, Error> {
        let column = self
            .columns
            .get(idx)
            .ok_or_else(|| Error::Protocol(format!("column index {idx} out of bounds")))?;
        let raw = self
            .ranges
            .get(idx)
            .ok_or_else(|| Error::Protocol(format!("column index {idx} out of bounds")))?
            .as_ref()
            .map(|r| &self.body.buffer()[r.clone()]);
        T::from_sql_nullable(&column.type_, raw).map_err(|e| Error::Protocol(e.to_string()))
    }

    pub fn try_get<'a, T: FromSql<'a>>(&'a self, name: &str) -> Result<T, Error> {
        let idx = self
            .columns
            .iter()
            .position(|c| c.name == name)
            .ok_or_else(|| Error::Protocol(format!("no column named {name}")))?;
        self.get(idx)
    }
}
