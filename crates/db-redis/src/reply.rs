use redis::Value;

/// A server reply as a tree (what the console shows).
#[derive(Clone, Debug, PartialEq)]
pub enum Reply {
    Nil,
    Int(i64),
    Double(f64),
    Bool(bool),
    Status(String),
    Bulk(Vec<u8>),
    Array(Vec<Reply>),
    Set(Vec<Reply>),
    Map(Vec<(Reply, Reply)>),
    Error(String),
}

impl Reply {
    pub fn from_value(v: Value) -> Reply {
        match v {
            Value::Nil => Reply::Nil,
            Value::Int(i) => Reply::Int(i),
            Value::BulkString(b) => Reply::Bulk(b),
            Value::Array(a) => Reply::Array(a.into_iter().map(Reply::from_value).collect()),
            Value::SimpleString(s) => Reply::Status(s),
            Value::Okay => Reply::Status("OK".into()),
            Value::Map(m) => Reply::Map(
                m.into_iter()
                    .map(|(k, v)| (Reply::from_value(k), Reply::from_value(v)))
                    .collect(),
            ),
            Value::Set(a) => Reply::Set(a.into_iter().map(Reply::from_value).collect()),
            Value::Double(d) => Reply::Double(d),
            Value::Boolean(b) => Reply::Bool(b),
            Value::VerbatimString { text, .. } => Reply::Bulk(text.into_bytes()),
            Value::BigNumber(n) => Reply::Status(format!("{n:?}")),
            Value::ServerError(e) => Reply::Error(format!("{e:?}")),
            other => Reply::Status(format!("{other:?}")),
        }
    }

    pub fn bytes(&self) -> Vec<u8> {
        match self {
            Reply::Bulk(b) => b.clone(),
            Reply::Status(s) | Reply::Error(s) => s.clone().into_bytes(),
            Reply::Int(i) => i.to_string().into_bytes(),
            Reply::Double(d) => d.to_string().into_bytes(),
            Reply::Bool(b) => b.to_string().into_bytes(),
            _ => Vec::new(),
        }
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }

    pub fn int(&self) -> i64 {
        match self {
            Reply::Int(i) => *i,
            other => other.text().trim().parse().unwrap_or(0),
        }
    }

    pub fn array(&self) -> &[Reply] {
        match self {
            Reply::Array(a) | Reply::Set(a) => a,
            _ => &[],
        }
    }
}
