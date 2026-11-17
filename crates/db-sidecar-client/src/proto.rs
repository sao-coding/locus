//! Wire format of the Go sidecar. Mirror of `sidecar/internal/proto`;
//! the frames are documented in `sidecar/PROTOCOL.md`.

use db_core::{Batch, BatchBuilder, ColumnMeta, DbError, ErrorKind, Param};
use std::io;
use tokio::io::{AsyncRead, AsyncReadExt};

// host -> sidecar
pub const QUERY: u8 = 1;
pub const CANCEL: u8 = 2;
pub const CREDIT: u8 = 3;
pub const KILL_SESSION: u8 = 4;
pub const RUN: u8 = 5;
pub const OPEN_SESSION: u8 = 6;
pub const CLOSE_SESSION: u8 = 7;
// sidecar -> host
pub const STARTED: u8 = 10;
pub const COLUMNS: u8 = 11;
pub const BATCH: u8 = 12;
pub const DONE: u8 = 13;
pub const ERROR: u8 = 14;
pub const CANCELLED: u8 = 15;
pub const AFFECTED: u8 = 16;
pub const READY: u8 = 20;

pub const VERSION: u16 = 1;
const NULL_LEN: u32 = 0xFFFF_FFFF;
/// The sidecar refuses larger frames from the host.
pub const MAX_OUTBOUND: usize = 64 << 20;
/// Largest frame the sidecar sends; anything bigger is a broken stream.
const MAX_INBOUND: u32 = 256 << 20;

pub fn frame(t: u8, qid: u32, payload: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(9 + payload.len());
    b.extend_from_slice(&(5 + payload.len() as u32).to_le_bytes());
    b.push(t);
    b.extend_from_slice(&qid.to_le_bytes());
    b.extend_from_slice(payload);
    b
}

pub fn query_frame(qid: u32, timeout_ms: u32, batch_rows: u32, sql: &str) -> Vec<u8> {
    let mut p = Vec::with_capacity(8 + sql.len());
    p.extend_from_slice(&timeout_ms.to_le_bytes());
    p.extend_from_slice(&batch_rows.to_le_bytes());
    p.extend_from_slice(sql.as_bytes());
    frame(QUERY, qid, &p)
}

/// `run` frame: a statement with bound parameters and / or on a pinned
/// session (`session` = qid of the `open_session` frame, 0 = pool). Values
/// travel as data, never as part of `sql`. `None` when a value or the frame
/// is too large.
pub fn run_frame(
    qid: u32,
    session: u32,
    timeout_ms: u32,
    batch_rows: u32,
    sql: &str,
    params: &[Param],
) -> Option<Vec<u8>> {
    let count = u16::try_from(params.len()).ok()?;
    let mut p =
        Vec::with_capacity(14 + sql.len() + params.iter().map(|a| 5 + a.len()).sum::<usize>());
    p.extend_from_slice(&session.to_le_bytes());
    p.extend_from_slice(&timeout_ms.to_le_bytes());
    p.extend_from_slice(&batch_rows.to_le_bytes());
    p.extend_from_slice(&count.to_le_bytes());
    for a in params {
        match a {
            Param::Null => p.push(0),
            Param::Text(s) => {
                p.push(1);
                p.extend_from_slice(&u32::try_from(s.len()).ok()?.to_le_bytes());
                p.extend_from_slice(s.as_bytes());
            }
            Param::Bytes(b) => {
                p.push(2);
                p.extend_from_slice(&u32::try_from(b.len()).ok()?.to_le_bytes());
                p.extend_from_slice(b);
            }
        }
    }
    p.extend_from_slice(sql.as_bytes());
    (p.len() + 9 <= MAX_OUTBOUND).then(|| frame(RUN, qid, &p))
}

trait ParamLen {
    fn len(&self) -> usize;
}

impl ParamLen for Param {
    fn len(&self) -> usize {
        match self {
            Param::Null => 0,
            Param::Text(s) => s.len(),
            Param::Bytes(b) => b.len(),
        }
    }
}

pub fn credit_frame(qid: u32, n: u32) -> Vec<u8> {
    frame(CREDIT, qid, &n.to_le_bytes())
}

/// Reads one frame into `buf` (reused between calls). `Ok(None)` is a clean
/// end of stream between frames.
pub async fn read_frame<R: AsyncRead + Unpin>(
    r: &mut R,
    buf: &mut Vec<u8>,
) -> io::Result<Option<(u8, u32)>> {
    let mut h = [0u8; 9];
    match r.read_exact(&mut h).await {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let len = u32::from_le_bytes(h[0..4].try_into().unwrap());
    if !(5..=MAX_INBOUND).contains(&len) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("bad frame length {len}"),
        ));
    }
    buf.clear();
    buf.resize(len as usize - 5, 0);
    r.read_exact(buf).await?;
    Ok(Some((
        h[4],
        u32::from_le_bytes(h[5..9].try_into().unwrap()),
    )))
}

fn bad(what: &str) -> DbError {
    DbError::new(
        ErrorKind::Internal,
        format!("malformed {what} frame from the sidecar"),
    )
}

struct Cursor<'a>(&'a [u8]);

impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Some(a)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2)
            .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    }
    fn str(&mut self) -> Option<String> {
        let n = self.u16()? as usize;
        self.take(n)
            .map(|b| String::from_utf8_lossy(b).into_owned())
    }
}

pub fn decode_u64(p: &[u8]) -> Result<u64, DbError> {
    p.get(..8)
        .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
        .ok_or_else(|| bad("u64"))
}

pub fn decode_columns(p: &[u8]) -> Result<Vec<ColumnMeta>, DbError> {
    let mut c = Cursor(p);
    let n = c.u16().ok_or_else(|| bad("columns"))?;
    let mut cols = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let (name, type_name, nullable) =
            (|| Some((c.str()?, c.str()?, c.u8()?)))().ok_or_else(|| bad("columns"))?;
        let nullable = match nullable {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        cols.push(ColumnMeta {
            name,
            type_name,
            nullable,
            origin: None,
        });
    }
    Ok(cols)
}

/// Decodes a batch frame straight into the columnar [`Batch`].
pub fn decode_batch(p: &[u8], columns: usize) -> Result<Batch, DbError> {
    let mut c = Cursor(p);
    let rows = c.u32().ok_or_else(|| bad("batch"))?;
    let mut b = BatchBuilder::new(columns);
    for _ in 0..rows {
        for _ in 0..columns {
            let len = c.u32().ok_or_else(|| bad("batch"))?;
            if len == NULL_LEN {
                b.push(None);
            } else {
                b.push(Some(c.take(len as usize).ok_or_else(|| bad("batch"))?));
            }
        }
        b.end_row();
    }
    if !c.0.is_empty() {
        return Err(bad("batch"));
    }
    Ok(b.finish())
}

pub fn decode_error(p: &[u8]) -> DbError {
    let mut c = Cursor(p);
    let (Some(kind), Some(code)) = (c.u8(), c.u32()) else {
        return bad("error");
    };
    let kind = match kind {
        1 => ErrorKind::Connect,
        2 => ErrorKind::Auth,
        3 => ErrorKind::Tls,
        4 => ErrorKind::Query,
        5 => ErrorKind::Timeout,
        _ => ErrorKind::Internal,
    };
    let e = DbError::new(kind, String::from_utf8_lossy(c.0).into_owned());
    if code != 0 {
        e.with_code(code as i64)
    } else {
        e
    }
}

/// What the ready frame says about the sidecar.
#[derive(Clone, Debug)]
pub struct ReadyInfo {
    pub driver: String,
    pub server_version: String,
}

pub fn decode_ready(p: &[u8]) -> Result<ReadyInfo, DbError> {
    let mut c = Cursor(p);
    let version = c.u16().ok_or_else(|| bad("ready"))?;
    if version != VERSION {
        return Err(DbError::new(
            ErrorKind::Internal,
            format!("sidecar speaks protocol {version}, host expects {VERSION}"),
        ));
    }
    let (driver, server_version) = (|| Some((c.str()?, c.str()?)))().ok_or_else(|| bad("ready"))?;
    Ok(ReadyInfo {
        driver,
        server_version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use db_core::Cell;

    #[test]
    fn frame_layout_matches_go_side() {
        // Same bytes as TestFrameLayout in sidecar/internal/proto.
        assert_eq!(
            frame(CREDIT, 0x01020304, &[9, 8]),
            [7, 0, 0, 0, 3, 4, 3, 2, 1, 9, 8]
        );
    }

    #[test]
    fn run_frame_layout_matches_go_side() {
        // session 3, timeout 9, batch 7, params: text "x", null, bytes ff 00; sql "q".
        let f = run_frame(
            5,
            3,
            9,
            7,
            "q",
            &[
                Param::Text("x".into()),
                Param::Null,
                Param::Bytes(vec![0xff, 0]),
            ],
        )
        .unwrap();
        let mut p = Vec::new();
        for v in [3u32, 9, 7] {
            p.extend_from_slice(&v.to_le_bytes());
        }
        p.extend_from_slice(&3u16.to_le_bytes());
        p.extend_from_slice(&[1, 1, 0, 0, 0, b'x', 0, 2, 2, 0, 0, 0, 0xff, 0, b'q']);
        assert_eq!(f, frame(RUN, 5, &p));
    }

    #[test]
    fn batch_with_null_empty_and_binary() {
        let mut p = 2u32.to_le_bytes().to_vec();
        for cell in [
            Some(&b"a"[..]),
            None,
            Some(&b""[..]),
            Some(&[0xff, 0, 0xfe][..]),
            Some("終".as_bytes()),
            None,
        ] {
            match cell {
                Some(c) => {
                    p.extend_from_slice(&(c.len() as u32).to_le_bytes());
                    p.extend_from_slice(c);
                }
                None => p.extend_from_slice(&NULL_LEN.to_le_bytes()),
            }
        }
        let b = decode_batch(&p, 3).unwrap();
        assert_eq!(b.rows(), 2);
        assert_eq!(b.cell(0, 0), Cell::Text("a"));
        assert_eq!(b.cell(0, 1), Cell::Null);
        assert_eq!(b.cell(0, 2), Cell::Text(""));
        assert_eq!(b.cell(1, 0), Cell::Bytes(&[0xff, 0, 0xfe]));
        assert_eq!(b.cell(1, 1), Cell::Text("終"));
        assert_eq!(b.cell(1, 2), Cell::Null);
        assert!(decode_batch(&p[..p.len() - 1], 3).is_err());
        assert!(decode_batch(&p, 2).is_err());
    }

    #[test]
    fn columns_error_and_ready() {
        let mut p = 1u16.to_le_bytes().to_vec();
        for s in ["id", "INT"] {
            p.extend_from_slice(&(s.len() as u16).to_le_bytes());
            p.extend_from_slice(s.as_bytes());
        }
        p.push(0);
        let cols = decode_columns(&p).unwrap();
        assert_eq!(
            (
                cols[0].name.as_str(),
                cols[0].type_name.as_str(),
                cols[0].nullable
            ),
            ("id", "INT", Some(false))
        );
        assert!(decode_columns(&p[..p.len() - 1]).is_err());

        let mut e = vec![2];
        e.extend_from_slice(&1045u32.to_le_bytes());
        e.extend_from_slice(b"Access denied");
        let e = decode_error(&e);
        assert_eq!(
            (e.kind, e.code, e.message.as_str()),
            (ErrorKind::Auth, Some(1045), "Access denied")
        );

        let mut r = VERSION.to_le_bytes().to_vec();
        for s in ["mysql", "11.8"] {
            r.extend_from_slice(&(s.len() as u16).to_le_bytes());
            r.extend_from_slice(s.as_bytes());
        }
        assert_eq!(decode_ready(&r).unwrap().server_version, "11.8");
    }
}
