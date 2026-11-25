//! The MCP endpoint: an rmcp `ServerHandler` over the tools, served as
//! Streamable HTTP (stateless, JSON responses) on 127.0.0.1 behind
//! [`crate::guard`].

use crate::guard;
use crate::host::Host;
use crate::tools::{Call, Core};
use bytes::Bytes;
use http_body_util::{BodyExt as _, Full};
use hyper::{Request, Response};
use parking_lot::{Mutex, RwLock};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    InitializeRequestParams, InitializeResult, ListToolsResult, PaginatedRequestParams,
    ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::RequestContext;
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager,
};
use rmcp::{ErrorData, RoleServer, ServerHandler};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// The handler rmcp calls; cheap to clone (one per request in stateless mode).
#[derive(Clone)]
pub struct Mcp {
    core: Arc<Core>,
    /// Client name from `initialize`, by User-Agent (stateless requests do
    /// not repeat it).
    clients: Arc<Mutex<HashMap<String, String>>>,
}

fn schema(props: Value, required: &[&str]) -> Arc<Map<String, Value>> {
    let v = json!({ "type": "object", "properties": props, "required": required });
    match v {
        Value::Object(m) => Arc::new(m),
        _ => unreachable!(),
    }
}

/// The tools, with short descriptions (a client reads them on every session).
pub fn tools() -> Vec<Tool> {
    let conn = json!({"type": "string", "description": "Connection id or name"});
    let schema_p = json!({"type": "string", "description": "Default: current schema"});
    vec![
        Tool::new(
            "list_connections",
            "Connections you may use: id, name, kind, env, access.",
            schema(json!({}), &[]),
        ),
        Tool::new(
            "list_objects",
            "No schema: list schemas. With schema: its tables, views and routines.",
            schema(
                json!({
                    "connection": conn,
                    "schema": {"type": "string"},
                    "kind": {"type": "string", "enum": ["all", "table", "view", "routine"]}
                }),
                &["connection"],
            ),
        ),
        Tool::new(
            "describe_table",
            "Columns (type, null, default), PK, indexes and FKs of a table or view.",
            schema(
                json!({"connection": conn, "table": {"type": "string"}, "schema": schema_p}),
                &["connection", "table"],
            ),
        ),
        Tool::new(
            "run_sql",
            "Run SQL (one or more statements). Result: TSV per statement. max_rows default 200, max 5000.",
            schema(
                json!({
                    "connection": conn,
                    "sql": {"type": "string"},
                    "max_rows": {"type": "integer", "minimum": 1, "maximum": 5000}
                }),
                &["connection", "sql"],
            ),
        ),
        Tool::new(
            "explain",
            "Query plan of one statement (not executed).",
            schema(
                json!({"connection": conn, "sql": {"type": "string"}}),
                &["connection", "sql"],
            ),
        ),
        Tool::new(
            "get_ddl",
            "CREATE statement of a table, view or routine.",
            schema(
                json!({"connection": conn, "object": {"type": "string"}, "schema": schema_p}),
                &["connection", "object"],
            ),
        ),
    ]
}

fn user_agent(ctx: &RequestContext<RoleServer>) -> String {
    ctx.extensions
        .get::<hyper::http::request::Parts>()
        .and_then(|p| p.headers.get(hyper::header::USER_AGENT))
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string()
}

impl Mcp {
    pub fn new(core: Arc<Core>) -> Self {
        Mcp {
            core,
            clients: Default::default(),
        }
    }

    fn client_name(&self, ctx: &RequestContext<RoleServer>) -> String {
        let ua = user_agent(ctx);
        if let Some(n) = self.clients.lock().get(&ua) {
            return n.clone();
        }
        let first = ua.split([' ', '/']).next().unwrap_or("").trim();
        if first.is_empty() {
            "MCP client".into()
        } else {
            first.chars().take(40).collect()
        }
    }

    /// Runs one tool by name.
    pub async fn dispatch(
        &self,
        name: &str,
        args: &Map<String, Value>,
        call: &Call<'_>,
    ) -> Result<String, String> {
        let c = &self.core;
        match name {
            "list_connections" => c.list_connections(),
            "list_objects" => c.list_objects(args, call).await,
            "describe_table" => c.describe_table(args, call).await,
            "run_sql" => c.run_sql(args, call).await,
            "explain" => c.explain(args, call).await,
            "get_ddl" => c.get_ddl(args, call).await,
            other => Err(format!("unknown tool '{other}'")),
        }
    }
}

impl ServerHandler for Mcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("locus", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Locus database tools. Call list_connections first; pass a connection id or name to the others. Results are TSV.",
            )
    }

    async fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<InitializeResult, ErrorData> {
        let name = request.client_info.name.trim();
        if !name.is_empty() {
            let mut m = self.clients.lock();
            if m.len() > 64 {
                m.clear();
            }
            m.insert(user_agent(&context), name.chars().take(60).collect());
        }
        context.peer.set_peer_info(request.clone());
        self.negotiate_initialize(&request)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(tools()))
    }

    fn get_tool(&self, name: &str) -> Option<Tool> {
        tools().into_iter().find(|t| t.name == name)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let client = self.client_name(&context);
        let args = request.arguments.unwrap_or_default();
        let call = Call {
            client: &client,
            ct: &context.ct,
        };
        let r = match self.dispatch(&request.name, &args, &call).await {
            Ok(text) => CallToolResult::success(vec![ContentBlock::text(text)]),
            Err(e) => CallToolResult::error(vec![ContentBlock::text(e)]),
        };
        Ok(r.into())
    }
}

type Body = http_body_util::combinators::BoxBody<Bytes, std::convert::Infallible>;

fn plain(status: hyper::StatusCode, text: &str) -> Response<Body> {
    let mut r = Response::new(Full::new(Bytes::from(text.to_string())).boxed());
    *r.status_mut() = status;
    r.headers_mut().insert(
        hyper::header::CONTENT_TYPE,
        hyper::header::HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if status == hyper::StatusCode::UNAUTHORIZED {
        r.headers_mut().insert(
            hyper::header::WWW_AUTHENTICATE,
            hyper::header::HeaderValue::from_static("Bearer"),
        );
    }
    r
}

/// A running server. Dropping it does not stop it: call [`Running::stop`].
pub struct Running {
    pub port: u16,
    ct: CancellationToken,
    token: Arc<RwLock<String>>,
    core: Arc<Core>,
}

impl Running {
    pub fn url(&self) -> String {
        format!("http://127.0.0.1:{}{}", self.port, guard::PATH)
    }

    /// Takes effect for the next request.
    pub fn set_token(&self, token: String) {
        *self.token.write() = token;
    }

    /// Stops listening, ends running calls and closes MCP's own connections.
    pub fn stop(&self) {
        self.ct.cancel();
        let core = self.core.clone();
        core.rt.clone().spawn(async move { core.close_all().await });
    }
}

/// Binds 127.0.0.1:`port` (never another address) and serves until
/// [`Running::stop`]. A busy port is an error, never another port.
pub fn start(
    rt: &tokio::runtime::Handle,
    port: u16,
    token: String,
    host: Arc<dyn Host>,
) -> Result<Running, String> {
    let core = Arc::new(Core::new(host, rt.clone()));
    start_with(rt, port, token, core)
}

pub fn start_with(
    rt: &tokio::runtime::Handle,
    port: u16,
    token: String,
    core: Arc<Core>,
) -> Result<Running, String> {
    if token.is_empty() {
        return Err("no token".into());
    }
    let std_listener =
        std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).map_err(|e| {
            if e.kind() == std::io::ErrorKind::AddrInUse {
                format!("port {port} is already in use")
            } else {
                format!("cannot listen on 127.0.0.1:{port}: {e}")
            }
        })?;
    std_listener
        .set_nonblocking(true)
        .map_err(|e| e.to_string())?;
    let port = std_listener.local_addr().map_err(|e| e.to_string())?.port();
    let listener = {
        let _g = rt.enter();
        tokio::net::TcpListener::from_std(std_listener).map_err(|e| e.to_string())?
    };
    let ct = CancellationToken::new();
    let token = Arc::new(RwLock::new(token));
    let handler = Mcp::new(core.clone());
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_allowed_hosts([
            format!("127.0.0.1:{port}"),
            format!("localhost:{port}"),
            format!("[::1]:{port}"),
        ])
        .with_allowed_origins([
            "http://localhost:*",
            "http://127.0.0.1:*",
            "http://[::1]:*",
            "https://localhost:*",
            "https://127.0.0.1:*",
            "https://[::1]:*",
        ])
        .with_max_request_body_bytes(guard::MAX_BODY)
        .with_cancellation_token(ct.child_token());
    let service: StreamableHttpService<Mcp, LocalSessionManager> = StreamableHttpService::new(
        move || Ok(handler.clone()),
        Arc::new(LocalSessionManager::default()),
        config,
    );
    let service = Arc::new(service);
    let (ct2, token2) = (ct.clone(), token.clone());
    rt.spawn(async move {
        loop {
            let stream = tokio::select! {
                _ = ct2.cancelled() => break,
                r = listener.accept() => match r {
                    Ok((s, _)) => s,
                    Err(e) => {
                        log::warn!("mcp accept: {e}");
                        continue;
                    }
                },
            };
            let (service, token, ct) = (service.clone(), token2.clone(), ct2.clone());
            tokio::spawn(async move {
                let svc = hyper::service::service_fn(move |req: Request<hyper::body::Incoming>| {
                    let (service, token) = (service.clone(), token.clone());
                    async move {
                        let t = token.read().clone();
                        if let Err(r) = guard::check(req.uri().path(), req.headers(), port, &t) {
                            return Ok::<_, std::convert::Infallible>(plain(r.status, r.message));
                        }
                        Ok(service.handle(req).await)
                    }
                });
                let conn = hyper::server::conn::http1::Builder::new()
                    .serve_connection(hyper_util::rt::TokioIo::new(stream), svc);
                tokio::select! {
                    _ = conn => {}
                    _ = ct.cancelled() => {}
                }
            });
        }
    });
    Ok(Running {
        port,
        ct,
        token,
        core,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_list_is_small() {
        let t = tools();
        let names: Vec<&str> = t.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(
            names,
            [
                "list_connections",
                "list_objects",
                "describe_table",
                "run_sql",
                "explain",
                "get_ddl"
            ]
        );
        let json = serde_json::to_string(&t).unwrap();
        // Descriptions stay short: the whole list is well under 2 KB.
        assert!(json.len() < 2200, "{}", json.len());
        for tool in &t {
            assert!(tool.description.as_deref().unwrap_or("").len() <= 100);
        }
    }
}
