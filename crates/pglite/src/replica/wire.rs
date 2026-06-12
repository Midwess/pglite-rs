use std::io::{Read, Write};
use std::net::TcpStream;
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use fallible_iterator::FallibleIterator;
use postgres_protocol::authentication::md5_hash;
use postgres_protocol::authentication::sasl::{ChannelBinding, ScramSha256, SCRAM_SHA_256};
use postgres_protocol::message::backend::{Header, Message};
use postgres_protocol::message::frontend;

use super::ReplicaConfig;
use crate::error::Error;

const PG_EPOCH_MICROS: i64 = 946_684_800_000_000;

pub(crate) enum Stream {
    Tcp(TcpStream),
    #[cfg(unix)]
    Unix(UnixStream),
}

impl Stream {
    fn connect(config: &ReplicaConfig) -> Result<Stream, Error> {
        #[cfg(unix)]
        if config.host.starts_with('/') {
            let path = format!("{}/.s.PGSQL.{}", config.host, config.port);
            return Ok(Stream::Unix(UnixStream::connect(path)?));
        }
        use std::net::ToSocketAddrs;
        let addr = (config.host.as_str(), config.port)
            .to_socket_addrs()?
            .next()
            .ok_or_else(|| Error::Upstream(format!("cannot resolve host {}", config.host)))?;
        Ok(Stream::Tcp(TcpStream::connect_timeout(
            &addr,
            config.read_timeout.max(Duration::from_secs(1)),
        )?))
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> Result<(), Error> {
        match self {
            Stream::Tcp(s) => s.set_read_timeout(timeout)?,
            #[cfg(unix)]
            Stream::Unix(s) => s.set_read_timeout(timeout)?,
        }
        Ok(())
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Stream::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.read(buf),
        }
    }

    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        match self {
            Stream::Tcp(s) => s.write_all(buf),
            #[cfg(unix)]
            Stream::Unix(s) => s.write_all(buf),
        }
    }

    fn shutdown(&self) {
        match self {
            Stream::Tcp(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
            #[cfg(unix)]
            Stream::Unix(s) => {
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        }
    }
}

#[derive(Debug)]
pub(crate) enum ReplMsg {
    XLogData { data: Bytes },
    Keepalive { wal_end: u64, reply_requested: bool },
    CopyDone,
}

pub(crate) struct ReplConn {
    stream: Stream,
    buf: BytesMut,
}

impl ReplConn {
    pub(crate) fn connect_and_auth(
        config: &ReplicaConfig,
        replication: bool,
    ) -> Result<ReplConn, Error> {
        let stream = Stream::connect(config)?;
        let mut conn = ReplConn {
            stream,
            buf: BytesMut::new(),
        };

        let mut params: Vec<(&str, &str)> = vec![
            ("user", config.user.as_str()),
            ("database", config.database.as_str()),
            ("application_name", config.application_name.as_str()),
        ];
        if replication {
            params.push(("replication", "database"));
        }
        let mut wire = BytesMut::new();
        frontend::startup_message(params, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        conn.stream.write_all(&wire)?;

        loop {
            match conn.read_message()? {
                Message::AuthenticationOk => {}
                Message::AuthenticationCleartextPassword => {
                    let mut wire = BytesMut::new();
                    frontend::password_message(config.password.as_bytes(), &mut wire)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    conn.stream.write_all(&wire)?;
                }
                Message::AuthenticationMd5Password(body) => {
                    let hashed = md5_hash(
                        config.user.as_bytes(),
                        config.password.as_bytes(),
                        body.salt(),
                    );
                    let mut wire = BytesMut::new();
                    frontend::password_message(hashed.as_bytes(), &mut wire)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    conn.stream.write_all(&wire)?;
                }
                Message::AuthenticationSasl(body) => {
                    let mut has_scram = false;
                    let mut mechanisms = body.mechanisms();
                    while let Some(m) = mechanisms
                        .next()
                        .map_err(|e| Error::Protocol(e.to_string()))?
                    {
                        if m == SCRAM_SHA_256 {
                            has_scram = true;
                        }
                    }
                    if !has_scram {
                        return Err(Error::Upstream(
                            "upstream offers no supported SASL mechanism (need SCRAM-SHA-256)"
                                .into(),
                        ));
                    }
                    let mut scram =
                        ScramSha256::new(config.password.as_bytes(), ChannelBinding::unsupported());
                    let mut wire = BytesMut::new();
                    frontend::sasl_initial_response(SCRAM_SHA_256, scram.message(), &mut wire)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    conn.stream.write_all(&wire)?;

                    match conn.read_message()? {
                        Message::AuthenticationSaslContinue(body) => {
                            scram
                                .update(body.data())
                                .map_err(|e| Error::Upstream(e.to_string()))?;
                            let mut wire = BytesMut::new();
                            frontend::sasl_response(scram.message(), &mut wire)
                                .map_err(|e| Error::Protocol(e.to_string()))?;
                            conn.stream.write_all(&wire)?;
                        }
                        Message::ErrorResponse(body) => {
                            return Err(Error::from_error_fields(body.fields()))
                        }
                        _ => return Err(Error::Protocol("unexpected SASL flow message".into())),
                    }
                    match conn.read_message()? {
                        Message::AuthenticationSaslFinal(body) => {
                            scram
                                .finish(body.data())
                                .map_err(|e| Error::Upstream(e.to_string()))?;
                        }
                        Message::ErrorResponse(body) => {
                            return Err(Error::from_error_fields(body.fields()))
                        }
                        _ => return Err(Error::Protocol("unexpected SASL flow message".into())),
                    }
                }
                Message::ParameterStatus(_)
                | Message::BackendKeyData(_)
                | Message::NoticeResponse(_) => {}
                Message::ReadyForQuery(_) => return Ok(conn),
                Message::ErrorResponse(body) => {
                    return Err(Error::from_error_fields(body.fields()))
                }
                _ => return Err(Error::Protocol("unexpected message during startup".into())),
            }
        }
    }

    fn read_message(&mut self) -> Result<Message, Error> {
        loop {
            if let Some(msg) =
                Message::parse(&mut self.buf).map_err(|e| Error::Protocol(e.to_string()))?
            {
                return Ok(msg);
            }
            self.fill()?;
        }
    }

    fn fill(&mut self) -> Result<(), Error> {
        let mut chunk = [0u8; 8192];
        let n = self.stream.read(&mut chunk)?;
        if n == 0 {
            return Err(Error::Upstream("connection closed by upstream".into()));
        }
        self.buf.extend_from_slice(&chunk[..n]);
        Ok(())
    }

    pub(crate) fn simple_query(&mut self, sql: &str) -> Result<Vec<Vec<Option<String>>>, Error> {
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        self.stream.write_all(&wire)?;

        let mut rows = Vec::new();
        let mut error: Option<Error> = None;
        loop {
            match self.read_message()? {
                Message::DataRow(body) => {
                    let buffer = body.buffer();
                    let mut values = Vec::new();
                    let mut ranges = body.ranges();
                    while let Some(range) =
                        ranges.next().map_err(|e| Error::Protocol(e.to_string()))?
                    {
                        values.push(range.map(|r| {
                            String::from_utf8_lossy(&buffer[r.start..r.end]).into_owned()
                        }));
                    }
                    rows.push(values);
                }
                Message::RowDescription(_)
                | Message::CommandComplete(_)
                | Message::EmptyQueryResponse
                | Message::ParameterStatus(_)
                | Message::NoticeResponse(_) => {}
                Message::ErrorResponse(body) => {
                    error = Some(Error::from_error_fields(body.fields()));
                }
                Message::ReadyForQuery(_) => {
                    return match error {
                        Some(e) => Err(e),
                        None => Ok(rows),
                    };
                }
                _ => {
                    return Err(Error::Protocol(
                        "unexpected message during simple query".into(),
                    ))
                }
            }
        }
    }

    pub(crate) fn copy_out<F>(&mut self, sql: &str, mut on_chunk: F) -> Result<(), Error>
    where
        F: FnMut(&[u8]) -> Result<(), Error>,
    {
        let mut wire = BytesMut::new();
        frontend::query(sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        self.stream.write_all(&wire)?;

        let mut error: Option<Error> = None;
        loop {
            match self.read_message()? {
                Message::CopyOutResponse(_) => {}
                Message::CopyData(body) => {
                    if error.is_none() {
                        if let Err(e) = on_chunk(body.data()) {
                            error = Some(e);
                        }
                    }
                }
                Message::CopyDone
                | Message::CommandComplete(_)
                | Message::ParameterStatus(_)
                | Message::NoticeResponse(_) => {}
                Message::ErrorResponse(body) => {
                    error = Some(Error::from_error_fields(body.fields()));
                }
                Message::ReadyForQuery(_) => {
                    return match error {
                        Some(e) => Err(e),
                        None => Ok(()),
                    };
                }
                _ => return Err(Error::Protocol("unexpected message during COPY OUT".into())),
            }
        }
    }

    pub(crate) fn start_replication(
        &mut self,
        slot: &str,
        start: super::Lsn,
        publication: &str,
    ) -> Result<(), Error> {
        let sql = format!(
            "START_REPLICATION SLOT \"{}\" LOGICAL {} (proto_version '1', publication_names '\"{}\"')",
            slot.replace('"', "\"\""),
            start.to_pg_str(),
            publication.replace('"', "\"\"")
        );
        let mut wire = BytesMut::new();
        frontend::query(&sql, &mut wire).map_err(|e| Error::Protocol(e.to_string()))?;
        self.stream.write_all(&wire)?;

        loop {
            let header =
                match Header::parse(&self.buf).map_err(|e| Error::Protocol(e.to_string()))? {
                    Some(h) if self.buf.len() > h.len() as usize => h,
                    _ => {
                        self.fill()?;
                        continue;
                    }
                };
            match header.tag() {
                b'W' => {
                    let _ = self.buf.split_to(1 + header.len() as usize);
                    return Ok(());
                }
                b'E' => {
                    let msg = Message::parse(&mut self.buf)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                    match msg {
                        Some(Message::ErrorResponse(body)) => {
                            return Err(Error::from_error_fields(body.fields()))
                        }
                        _ => return Err(Error::Protocol("malformed error response".into())),
                    }
                }
                b'N' => {
                    let _ = Message::parse(&mut self.buf)
                        .map_err(|e| Error::Protocol(e.to_string()))?;
                }
                other => {
                    return Err(Error::Protocol(format!(
                        "unexpected message {} awaiting CopyBothResponse",
                        other as char
                    )))
                }
            }
        }
    }

    pub(crate) fn set_stream_timeout(&self, timeout: Duration) -> Result<(), Error> {
        self.stream.set_read_timeout(Some(timeout))
    }

    pub(crate) fn wal_sender_timeout_ms(&mut self) -> Result<Option<u64>, Error> {
        let rows =
            self.simple_query("SELECT setting FROM pg_settings WHERE name = 'wal_sender_timeout'")?;
        Ok(rows
            .first()
            .and_then(|r| r.first())
            .and_then(|v| v.as_deref())
            .and_then(|s| s.parse::<u64>().ok())
            .filter(|ms| *ms > 0))
    }

    pub(crate) fn read_copy_message(&mut self) -> Result<Option<ReplMsg>, Error> {
        loop {
            if let Some(msg) =
                Message::parse(&mut self.buf).map_err(|e| Error::Protocol(e.to_string()))?
            {
                match msg {
                    Message::CopyData(body) => {
                        let payload = body.data();
                        match payload.first() {
                            Some(b'w') => {
                                if payload.len() < 25 {
                                    return Err(Error::Protocol("truncated XLogData".into()));
                                }
                                return Ok(Some(ReplMsg::XLogData {
                                    data: Bytes::copy_from_slice(&payload[25..]),
                                }));
                            }
                            Some(b'k') => {
                                if payload.len() < 18 {
                                    return Err(Error::Protocol("truncated keepalive".into()));
                                }
                                return Ok(Some(ReplMsg::Keepalive {
                                    wal_end: u64::from_be_bytes(payload[1..9].try_into().unwrap()),
                                    reply_requested: payload[17] != 0,
                                }));
                            }
                            other => {
                                return Err(Error::Protocol(format!(
                                    "unknown replication submessage: {:?}",
                                    other.map(|b| *b as char)
                                )))
                            }
                        }
                    }
                    Message::CopyDone => return Ok(Some(ReplMsg::CopyDone)),
                    Message::ErrorResponse(body) => {
                        return Err(Error::from_error_fields(body.fields()))
                    }
                    Message::ParameterStatus(_) | Message::NoticeResponse(_) => {}
                    _ => {
                        return Err(Error::Protocol(
                            "unexpected message on replication stream".into(),
                        ))
                    }
                }
            } else {
                let mut chunk = [0u8; 8192];
                match self.stream.read(&mut chunk) {
                    Ok(0) => return Err(Error::Upstream("connection closed by upstream".into())),
                    Ok(n) => self.buf.extend_from_slice(&chunk[..n]),
                    Err(e)
                        if e.kind() == std::io::ErrorKind::WouldBlock
                            || e.kind() == std::io::ErrorKind::TimedOut =>
                    {
                        return Ok(None)
                    }
                    Err(e) => return Err(Error::Io(e)),
                }
            }
        }
    }

    pub(crate) fn send_status(&mut self, watermark: super::Lsn, reply: bool) -> Result<(), Error> {
        let now_micros = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_micros() as i64)
            .unwrap_or(0);
        let mut payload = Vec::with_capacity(34);
        payload.push(b'r');
        payload.extend_from_slice(&watermark.0.to_be_bytes());
        payload.extend_from_slice(&watermark.0.to_be_bytes());
        payload.extend_from_slice(&watermark.0.to_be_bytes());
        payload.extend_from_slice(&(now_micros - PG_EPOCH_MICROS).to_be_bytes());
        payload.push(u8::from(reply));

        let mut wire = Vec::with_capacity(payload.len() + 5);
        wire.push(b'd');
        wire.extend_from_slice(&((4 + payload.len()) as u32).to_be_bytes());
        wire.extend_from_slice(&payload);
        self.stream.write_all(&wire)?;
        Ok(())
    }

    pub(crate) fn terminate(&mut self) {
        let mut wire = BytesMut::new();
        frontend::terminate(&mut wire);
        let _ = self.stream.write_all(&wire);
        self.stream.shutdown();
    }
}
