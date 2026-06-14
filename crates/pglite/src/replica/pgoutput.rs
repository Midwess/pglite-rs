use crate::error::Error;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RelColumn {
    pub flags: u8,
    pub name: String,
    pub type_oid: u32,
    pub type_modifier: i32,
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum CellValue {
    Null,
    UnchangedToast,
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TupleData(pub Vec<CellValue>);

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PgOutputMsg {
    Begin {
        final_lsn: u64,
        commit_ts: i64,
        xid: u32,
    },
    Commit {
        commit_lsn: u64,
        end_lsn: u64,
        commit_ts: i64,
    },
    Relation {
        rel_id: u32,
        namespace: String,
        name: String,
        replica_identity: u8,
        columns: Vec<RelColumn>,
    },
    Insert {
        rel_id: u32,
        new: TupleData,
    },
    Update {
        rel_id: u32,
        key: Option<TupleData>,
        old: Option<TupleData>,
        new: TupleData,
    },
    Delete {
        rel_id: u32,
        key: Option<TupleData>,
        old: Option<TupleData>,
    },
    Truncate {
        rel_ids: Vec<u32>,
    },
    Message {
        prefix: String,
        content: Vec<u8>,
    },
    Other,
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Cursor<'a> {
        Cursor { buf, pos: 0 }
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.pos + n > self.buf.len() {
            return Err(Error::Protocol("truncated pgoutput message".into()));
        }
        let out = &self.buf[self.pos..self.pos + n];
        self.pos += n;
        Ok(out)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn cstr(&mut self) -> Result<String, Error> {
        let rest = &self.buf[self.pos..];
        let nul = rest
            .iter()
            .position(|b| *b == 0)
            .ok_or_else(|| Error::Protocol("unterminated string in pgoutput message".into()))?;
        let s = std::str::from_utf8(&rest[..nul])
            .map_err(|_| Error::Protocol("non-utf8 string in pgoutput message".into()))?
            .to_string();
        self.pos += nul + 1;
        Ok(s)
    }

    fn tuple(&mut self) -> Result<TupleData, Error> {
        let ncols = self.u16()? as usize;
        let mut cells = Vec::with_capacity(ncols);
        for _ in 0..ncols {
            let kind = self.u8()?;
            cells.push(match kind {
                b'n' => CellValue::Null,
                b'u' => CellValue::UnchangedToast,
                b't' => {
                    let len = self.i32()?;
                    if len < 0 {
                        return Err(Error::Protocol("negative tuple cell length".into()));
                    }
                    let bytes = self.take(len as usize)?;
                    CellValue::Text(
                        std::str::from_utf8(bytes)
                            .map_err(|_| Error::Protocol("non-utf8 tuple cell".into()))?
                            .to_string(),
                    )
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unsupported tuple cell kind: {}",
                        other as char
                    )))
                }
            });
        }
        Ok(TupleData(cells))
    }
}

pub(crate) fn decode(data: &[u8]) -> Result<PgOutputMsg, Error> {
    let mut c = Cursor::new(data);
    let tag = c.u8()?;
    match tag {
        b'B' => {
            let final_lsn = c.u64()?;
            let commit_ts = c.i64()?;
            let xid = c.u32()?;
            Ok(PgOutputMsg::Begin {
                final_lsn,
                commit_ts,
                xid,
            })
        }
        b'C' => {
            let _flags = c.u8()?;
            let commit_lsn = c.u64()?;
            let end_lsn = c.u64()?;
            let commit_ts = c.i64()?;
            Ok(PgOutputMsg::Commit {
                commit_lsn,
                end_lsn,
                commit_ts,
            })
        }
        b'R' => {
            let rel_id = c.u32()?;
            let namespace = c.cstr()?;
            let name = c.cstr()?;
            let replica_identity = c.u8()?;
            let ncols = c.u16()? as usize;
            let mut columns = Vec::with_capacity(ncols);
            for _ in 0..ncols {
                let flags = c.u8()?;
                let name = c.cstr()?;
                let type_oid = c.u32()?;
                let type_modifier = c.i32()?;
                columns.push(RelColumn {
                    flags,
                    name,
                    type_oid,
                    type_modifier,
                });
            }
            Ok(PgOutputMsg::Relation {
                rel_id,
                namespace,
                name,
                replica_identity,
                columns,
            })
        }
        b'I' => {
            let rel_id = c.u32()?;
            let marker = c.u8()?;
            if marker != b'N' {
                return Err(Error::Protocol("insert without new tuple".into()));
            }
            Ok(PgOutputMsg::Insert {
                rel_id,
                new: c.tuple()?,
            })
        }
        b'U' => {
            let rel_id = c.u32()?;
            let mut key = None;
            let mut old = None;
            let mut marker = c.u8()?;
            if marker == b'K' {
                key = Some(c.tuple()?);
                marker = c.u8()?;
            } else if marker == b'O' {
                old = Some(c.tuple()?);
                marker = c.u8()?;
            }
            if marker != b'N' {
                return Err(Error::Protocol("update without new tuple".into()));
            }
            Ok(PgOutputMsg::Update {
                rel_id,
                key,
                old,
                new: c.tuple()?,
            })
        }
        b'D' => {
            let rel_id = c.u32()?;
            let marker = c.u8()?;
            let (key, old) = match marker {
                b'K' => (Some(c.tuple()?), None),
                b'O' => (None, Some(c.tuple()?)),
                _ => return Err(Error::Protocol("delete without key or old tuple".into())),
            };
            Ok(PgOutputMsg::Delete { rel_id, key, old })
        }
        b'T' => {
            let nrels = c.u32()? as usize;
            let _options = c.u8()?;
            let mut rel_ids = Vec::with_capacity(nrels);
            for _ in 0..nrels {
                rel_ids.push(c.u32()?);
            }
            Ok(PgOutputMsg::Truncate { rel_ids })
        }
        b'M' => {
            let _flags = c.u8()?;
            let _lsn = c.u64()?;
            let prefix = c.cstr()?;
            let len = c.i32()?;
            if len < 0 {
                return Err(Error::Protocol("negative logical message length".into()));
            }
            let content = c.take(len as usize)?.to_vec();
            Ok(PgOutputMsg::Message { prefix, content })
        }
        b'O' | b'Y' => Ok(PgOutputMsg::Other),
        other => Err(Error::Protocol(format!(
            "unknown pgoutput message tag: {}",
            other as char
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cstr(s: &str) -> Vec<u8> {
        let mut v = s.as_bytes().to_vec();
        v.push(0);
        v
    }

    fn text_cell(s: &str) -> Vec<u8> {
        let mut v = vec![b't'];
        v.extend_from_slice(&(s.len() as i32).to_be_bytes());
        v.extend_from_slice(s.as_bytes());
        v
    }

    #[test]
    fn decode_begin() {
        let mut buf = vec![b'B'];
        buf.extend_from_slice(&0x16_B374D848u64.to_be_bytes());
        buf.extend_from_slice(&123_456_789i64.to_be_bytes());
        buf.extend_from_slice(&42u32.to_be_bytes());
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Begin {
                final_lsn: 0x16_B374D848,
                commit_ts: 123_456_789,
                xid: 42
            }
        );
    }

    #[test]
    fn decode_commit() {
        let mut buf = vec![b'C', 0];
        buf.extend_from_slice(&100u64.to_be_bytes());
        buf.extend_from_slice(&200u64.to_be_bytes());
        buf.extend_from_slice(&300i64.to_be_bytes());
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Commit {
                commit_lsn: 100,
                end_lsn: 200,
                commit_ts: 300
            }
        );
    }

    #[test]
    fn decode_relation() {
        let mut buf = vec![b'R'];
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.extend_from_slice(&cstr("public"));
        buf.extend_from_slice(&cstr("todos"));
        buf.push(b'd');
        buf.extend_from_slice(&2u16.to_be_bytes());
        buf.push(1);
        buf.extend_from_slice(&cstr("id"));
        buf.extend_from_slice(&23u32.to_be_bytes());
        buf.extend_from_slice(&(-1i32).to_be_bytes());
        buf.push(0);
        buf.extend_from_slice(&cstr("title"));
        buf.extend_from_slice(&25u32.to_be_bytes());
        buf.extend_from_slice(&(-1i32).to_be_bytes());
        let msg = decode(&buf).unwrap();
        assert_eq!(
            msg,
            PgOutputMsg::Relation {
                rel_id: 7,
                namespace: "public".into(),
                name: "todos".into(),
                replica_identity: b'd',
                columns: vec![
                    RelColumn {
                        flags: 1,
                        name: "id".into(),
                        type_oid: 23,
                        type_modifier: -1
                    },
                    RelColumn {
                        flags: 0,
                        name: "title".into(),
                        type_oid: 25,
                        type_modifier: -1
                    },
                ],
            }
        );
    }

    #[test]
    fn decode_insert_with_null_and_toast() {
        let mut buf = vec![b'I'];
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.push(b'N');
        buf.extend_from_slice(&3u16.to_be_bytes());
        buf.extend_from_slice(&text_cell("1"));
        buf.push(b'n');
        buf.push(b'u');
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Insert {
                rel_id: 7,
                new: TupleData(vec![
                    CellValue::Text("1".into()),
                    CellValue::Null,
                    CellValue::UnchangedToast
                ]),
            }
        );
    }

    #[test]
    fn decode_update_with_key() {
        let mut buf = vec![b'U'];
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.push(b'K');
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&text_cell("1"));
        buf.push(b'N');
        buf.extend_from_slice(&2u16.to_be_bytes());
        buf.extend_from_slice(&text_cell("2"));
        buf.extend_from_slice(&text_cell("hello"));
        let msg = decode(&buf).unwrap();
        assert_eq!(
            msg,
            PgOutputMsg::Update {
                rel_id: 7,
                key: Some(TupleData(vec![CellValue::Text("1".into())])),
                old: None,
                new: TupleData(vec![
                    CellValue::Text("2".into()),
                    CellValue::Text("hello".into())
                ]),
            }
        );
    }

    #[test]
    fn decode_update_plain() {
        let mut buf = vec![b'U'];
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.push(b'N');
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&text_cell("x"));
        let msg = decode(&buf).unwrap();
        assert_eq!(
            msg,
            PgOutputMsg::Update {
                rel_id: 7,
                key: None,
                old: None,
                new: TupleData(vec![CellValue::Text("x".into())]),
            }
        );
    }

    #[test]
    fn decode_delete_with_old() {
        let mut buf = vec![b'D'];
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.push(b'O');
        buf.extend_from_slice(&1u16.to_be_bytes());
        buf.extend_from_slice(&text_cell("9"));
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Delete {
                rel_id: 7,
                key: None,
                old: Some(TupleData(vec![CellValue::Text("9".into())])),
            }
        );
    }

    #[test]
    fn decode_truncate() {
        let mut buf = vec![b'T'];
        buf.extend_from_slice(&2u32.to_be_bytes());
        buf.push(0);
        buf.extend_from_slice(&7u32.to_be_bytes());
        buf.extend_from_slice(&9u32.to_be_bytes());
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Truncate {
                rel_ids: vec![7, 9]
            }
        );
    }

    #[test]
    fn decode_logical_message() {
        let mut buf = vec![b'M', 1];
        buf.extend_from_slice(&42u64.to_be_bytes());
        buf.extend_from_slice(&cstr("pglite_ddl"));
        buf.extend_from_slice(&3i32.to_be_bytes());
        buf.extend_from_slice(b"abc");
        assert_eq!(
            decode(&buf).unwrap(),
            PgOutputMsg::Message {
                prefix: "pglite_ddl".into(),
                content: b"abc".to_vec(),
            }
        );
    }

    #[test]
    fn decode_origin_and_type_skipped() {
        let mut origin = vec![b'O'];
        origin.extend_from_slice(&5u64.to_be_bytes());
        origin.extend_from_slice(&cstr("origin"));
        assert_eq!(decode(&origin).unwrap(), PgOutputMsg::Other);
        let mut ty = vec![b'Y'];
        ty.extend_from_slice(&600u32.to_be_bytes());
        ty.extend_from_slice(&cstr("public"));
        ty.extend_from_slice(&cstr("mytype"));
        assert_eq!(decode(&ty).unwrap(), PgOutputMsg::Other);
    }

    #[test]
    fn decode_truncated_fails() {
        let buf = vec![b'B', 0, 0];
        assert!(decode(&buf).is_err());
        assert!(decode(&[]).is_err());
        assert!(decode(b"Z").is_err());
    }
}
