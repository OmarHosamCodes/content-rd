//! Content R&D MCP server. `content-rd` serves; `content-rd bridge` runs the local Instagram bridge.
mod board;
mod instagram;
mod relay;
mod search;
mod tools;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{Path, Request, State as Ax};
use axum::http::{StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager};
use subtle::ConstantTimeEq;

pub type ToolResult<T> = Result<T, String>;

pub struct Config {
    pub port: u16,
    pub token: String,
    pub data_dir: PathBuf,
    pub base_url: String,
    pub searxng_url: Option<String>,
}

impl Config {
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> Result<Config, String> {
        let var = |k: &str| var(k).filter(|v| !v.trim().is_empty());
        let token = var("MCP_TOKEN").ok_or("MCP_TOKEN is required: set it to a long random string")?;
        let port = match var("PORT") {
            Some(p) => p.parse().map_err(|_| format!("PORT must be a number, got {p}"))?,
            None => 8787,
        };
        let base_url = var("BASE_URL")
            .or_else(|| var("RAILWAY_PUBLIC_DOMAIN").map(|d| format!("https://{d}")))
            .unwrap_or_else(|| format!("http://localhost:{port}"));
        Ok(Config {
            port,
            token,
            data_dir: var("DATA_DIR").unwrap_or_else(|| "boards".into()).into(),
            base_url: base_url.trim_end_matches('/').to_string(),
            searxng_url: var("SEARXNG_URL"),
        })
    }
}

pub struct State {
    pub cfg: Config,
    pub store: board::Store,
    pub relay: Arc<relay::Relay>,
    pub http: reqwest::Client,
}

async fn require_token(Ax(state): Ax<Arc<State>>, req: Request, next: Next) -> Response {
    let given = req.headers().get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer "));
    match given {
        Some(t) if bool::from(t.as_bytes().ct_eq(state.cfg.token.as_bytes())) => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "missing or wrong bearer token").into_response(),
    }
}

async fn board_page(Ax(state): Ax<Arc<State>>, Path(name): Path<String>) -> Response {
    if !board::valid_name(&name) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match state.store.load(&name) {
        Ok(Some(b)) => Html(board::render(&name, &b).into_string()).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    }
}

async fn bridge_next(Ax(state): Ax<Arc<State>>) -> Response {
    match state.relay.next_job().await {
        Some(job) => Json(job).into_response(),
        None => StatusCode::NO_CONTENT.into_response(),
    }
}

#[derive(serde::Deserialize)]
struct BridgeResult {
    ok: Option<serde_json::Value>,
    err: Option<String>,
}

async fn bridge_result(Ax(state): Ax<Arc<State>>, Path(id): Path<u64>, Json(r): Json<BridgeResult>) -> StatusCode {
    state.relay.finish(id, match r.err {
        Some(e) => Err(e),
        None => Ok(r.ok.unwrap_or_default()),
    });
    StatusCode::NO_CONTENT
}

pub fn build(cfg: Config, relay: Arc<relay::Relay>) -> ToolResult<Router> {
    // rmcp rejects unknown Host headers (DNS rebinding guard), so the public host must be listed
    let authority = cfg.base_url.split("://").last().unwrap_or_default().to_string();
    let host = authority.split(':').next().unwrap_or_default().to_string();
    let mcp_cfg = StreamableHttpServerConfig::default().with_allowed_hosts([authority, host, "localhost".into(), "127.0.0.1".into()]);
    let state = Arc::new(State { store: board::Store::new(cfg.data_dir.clone()), cfg, relay, http: search::client()? });
    let for_mcp = state.clone();
    let mcp = StreamableHttpService::new(move || Ok(tools::ContentRd::new(for_mcp.clone())), LocalSessionManager::default().into(), mcp_cfg);
    let private = Router::new()
        .nest_service("/mcp", mcp)
        .route("/bridge/next", get(bridge_next))
        .route("/bridge/result/{id}", post(bridge_result))
        .layer(middleware::from_fn_with_state(state.clone(), require_token));
    Ok(Router::new().route("/", get(|| async { "ok" })).route("/b/{board}", get(board_page)).merge(private).with_state(state))
}

#[tokio::main]
async fn main() {
    if let Err(e) = run().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn run() -> ToolResult<()> {
    let cfg = Config::from_vars(|k| std::env::var(k).ok())?;
    let (port, base) = (cfg.port, cfg.base_url.clone());
    // a comments job pages for about a minute; the bridge long-polls every 25s
    let app = build(cfg, Arc::new(relay::Relay::new(Duration::from_secs(180), Duration::from_secs(25))))?;
    let listener = tokio::net::TcpListener::bind(("0.0.0.0", port)).await.map_err(|e| format!("cannot listen on port {port}: {e}"))?;
    eprintln!("MCP: {base}/mcp   boards: {base}/b/<name>");
    axum::serve(listener, app).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode, header};
    use std::sync::Arc;
    use std::time::Duration;
    use tower::ServiceExt;

    const INIT: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

    fn cfg(dir: &std::path::Path) -> Config {
        Config { port: 0, token: "secret".into(), data_dir: dir.to_path_buf(), base_url: "http://localhost:8787".into(), searxng_url: None }
    }

    fn test_app(dir: &std::path::Path) -> (axum::Router, Arc<relay::Relay>) {
        let relay = Arc::new(relay::Relay::new(Duration::from_millis(500), Duration::from_millis(100)));
        (build(cfg(dir), relay.clone()).unwrap(), relay)
    }

    fn req(method: &str, uri: &str, token: Option<&str>, body: &str) -> Request<Body> {
        let mut r = Request::builder().method(method).uri(uri).header(header::HOST, "localhost").header(header::CONTENT_TYPE, "application/json").header(header::ACCEPT, "application/json, text/event-stream");
        if let Some(t) = token {
            r = r.header(header::AUTHORIZATION, format!("Bearer {t}"));
        }
        r.body(Body::from(body.to_string())).unwrap()
    }

    async fn status(app: &axum::Router, r: Request<Body>) -> StatusCode {
        app.clone().oneshot(r).await.unwrap().status()
    }

    async fn text(app: &axum::Router, r: Request<Body>) -> (StatusCode, String) {
        let res = app.clone().oneshot(r).await.unwrap();
        let code = res.status();
        (code, String::from_utf8(axum::body::to_bytes(res.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap())
    }

    #[tokio::test]
    async fn mcp_requires_token() {
        let dir = tempfile::tempdir().unwrap();
        let (app, _) = test_app(dir.path());
        assert_eq!(status(&app, req("POST", "/mcp", None, INIT)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, req("POST", "/mcp", Some("wrong"), INIT)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, req("POST", "/mcp", Some("secre"), INIT)).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, req("POST", "/mcp", Some("secret"), INIT)).await, StatusCode::OK);
    }

    #[tokio::test]
    async fn boards_are_public_and_safe() {
        let dir = tempfile::tempdir().unwrap();
        let (app, _) = test_app(dir.path());
        assert_eq!(text(&app, req("GET", "/", None, "")).await, (StatusCode::OK, "ok".to_string()));
        assert_eq!(status(&app, req("GET", "/b/nope", None, "")).await, StatusCode::NOT_FOUND);
        assert_eq!(status(&app, req("GET", "/b/..%2Fsecret", None, "")).await, StatusCode::NOT_FOUND);
        std::fs::write(dir.path().join("demo.json"), r#"{"comments":[{"pattern":"p","user":"u","text":"a real comment","likes":1}]}"#).unwrap();
        let (code, body) = text(&app, req("GET", "/b/demo", None, "")).await;
        assert_eq!(code, StatusCode::OK);
        assert!(body.contains("a real comment"));
    }

    #[tokio::test]
    async fn bridge_routes_relay_a_job() {
        let dir = tempfile::tempdir().unwrap();
        let (app, relay) = test_app(dir.path());
        assert_eq!(status(&app, req("GET", "/bridge/next", None, "")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, req("POST", "/bridge/result/1", None, "{}")).await, StatusCode::UNAUTHORIZED);
        assert_eq!(status(&app, req("GET", "/bridge/next", Some("secret"), "")).await, StatusCode::NO_CONTENT);

        let call = tokio::spawn(async move { relay.call("find_reels", serde_json::json!({"topic": "x"})).await });
        let (code, body) = text(&app, req("GET", "/bridge/next", Some("secret"), "")).await;
        assert_eq!(code, StatusCode::OK);
        let job: relay::Job = serde_json::from_str(&body).unwrap();
        assert_eq!((job.tool.as_str(), &job.args["topic"]), ("find_reels", &serde_json::json!("x")));
        let uri = format!("/bridge/result/{}", job.id);
        assert_eq!(status(&app, req("POST", &uri, Some("secret"), r#"{"ok":[1,2]}"#)).await, StatusCode::NO_CONTENT);
        assert_eq!(call.await.unwrap().unwrap(), serde_json::json!([1, 2]));
    }

    #[tokio::test]
    async fn bridge_error_result_reaches_the_caller() {
        let dir = tempfile::tempdir().unwrap();
        let (app, relay) = test_app(dir.path());
        status(&app, req("GET", "/bridge/next", Some("secret"), "")).await; // bridge is online
        let call = tokio::spawn(async move { relay.call("get_comments", serde_json::json!({})).await });
        let (_, body) = text(&app, req("GET", "/bridge/next", Some("secret"), "")).await;
        let job: relay::Job = serde_json::from_str(&body).unwrap();
        status(&app, req("POST", &format!("/bridge/result/{}", job.id), Some("secret"), r#"{"err":"Log in to Instagram"}"#)).await;
        assert_eq!(call.await.unwrap().unwrap_err(), "Log in to Instagram");
    }

    #[test]
    fn config_from_env() {
        let vars = |pairs: &'static [(&'static str, &'static str)]| move |k: &str| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string());
        assert!(Config::from_vars(vars(&[])).err().unwrap().contains("MCP_TOKEN"));
        assert!(Config::from_vars(vars(&[("MCP_TOKEN", "")])).err().unwrap().contains("MCP_TOKEN"));
        assert!(Config::from_vars(vars(&[("MCP_TOKEN", "t"), ("PORT", "abc")])).err().unwrap().contains("PORT"));
        let c = Config::from_vars(vars(&[("MCP_TOKEN", "t")])).unwrap();
        assert_eq!((c.port, c.base_url.as_str(), c.searxng_url.is_none()), (8787, "http://localhost:8787", true));
        let c = Config::from_vars(vars(&[("MCP_TOKEN", "t"), ("PORT", "9000"), ("RAILWAY_PUBLIC_DOMAIN", "x.up.railway.app"), ("SEARXNG_URL", "")])).unwrap();
        assert_eq!((c.base_url.as_str(), c.searxng_url.is_none()), ("https://x.up.railway.app", true));
        let c = Config::from_vars(vars(&[("MCP_TOKEN", "t"), ("BASE_URL", "https://rd.example.com/")])).unwrap();
        assert_eq!(c.base_url, "https://rd.example.com");
    }
}
