//! `content-rd bridge`: runs on the user's computer. Opens Chrome with its own profile (the user logs in to
//! Instagram in that window), takes Instagram jobs from the server and answers them. No cookie leaves this machine.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use chromiumoxide::cdp::js_protocol::runtime::{CallArgument, CallFunctionOnParams};
use chromiumoxide::{Browser, BrowserConfig, Page};
use futures_util::StreamExt;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::relay::Job;
use crate::{ToolResult, instagram};

const IG: &str = "https://www.instagram.com/";
const NOT_LOGGED_IN: &str = "Log in to Instagram in the bridge's Chrome window, then retry.";

const FETCH_JS: &str = "async (url) => {
    const r = await fetch(url, {headers: {'X-IG-App-ID': '936619743392459'}, credentials: 'include'});
    return await r.text();
}";

/// Everything the search page already loaded, plus the two search APIs. Shapes vary, so the Rust side just walks all of it.
const SEARCH_JS: &str = "async (topic) => {
    if (location.pathname.startsWith('/accounts/login')) return null;
    const out = [...document.querySelectorAll('script[type=\"application/json\"]')].map(e => e.textContent);
    const q = encodeURIComponent(topic);
    const tag = encodeURIComponent(topic.replace(/[\\s#]/g, ''));
    for (const path of ['/api/v1/fbsearch/web/top_serp/?query=' + q, '/api/v1/tags/web_info/?tag_name=' + tag]) {
        try {
            const r = await fetch(path, {headers: {'X-IG-App-ID': '936619743392459'}, credentials: 'include'});
            out.push(await r.text());
        } catch (e) {}
    }
    return out;
}";

/// First Chrome-like browser on PATH. chromiumoxide's own detection misses some installs (e.g. Chrome Canary on Fedora).
fn find_chrome(path: &str) -> Option<PathBuf> {
    let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "chrome", "chrome.exe"];
    names.iter().flat_map(|n| std::env::split_paths(path).map(move |d| d.join(n))).find(|p| p.is_file())
}

pub struct Settings {
    pub server: String,
    pub token: String,
    pub profile: PathBuf,
    pub chrome: Option<PathBuf>,
}

impl Settings {
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> ToolResult<Settings> {
        let var = |k: &str| var(k).filter(|v| !v.trim().is_empty());
        let server = var("SERVER_URL").ok_or("SERVER_URL is required, e.g. https://your-app.up.railway.app")?;
        let host = reqwest::Url::parse(&server).ok().and_then(|u| u.host_str().map(str::to_string));
        if !server.starts_with("https://") && !matches!(host.as_deref(), Some("localhost" | "127.0.0.1")) {
            return Err("SERVER_URL must be https:// (the token is sent with every request)".into());
        }
        let profile = match var("BRIDGE_PROFILE") {
            Some(p) => p.into(),
            None => PathBuf::from(var("HOME").ok_or("HOME is not set; set BRIDGE_PROFILE to a folder for the Chrome profile")?).join(".local/share/content-rd/chrome"),
        };
        Ok(Settings {
            server: server.trim_end_matches('/').to_string(),
            token: var("MCP_TOKEN").ok_or("MCP_TOKEN is required: the same token the server uses")?,
            profile,
            chrome: var("CHROME").map(Into::into).or_else(|| find_chrome(&var("PATH").unwrap_or_default())),
        })
    }
}

async fn call_js<T: serde::de::DeserializeOwned>(page: &Page, function: &str, arg: &str) -> ToolResult<T> {
    let closed = |e: &dyn std::fmt::Display| format!("Chrome did not answer ({e}). Is the bridge's Chrome window still open?");
    let params = CallFunctionOnParams::builder()
        .function_declaration(function)
        .argument(CallArgument::builder().value(json!(arg)).build())
        .await_promise(true)
        .build()
        .map_err(|e| closed(&e))?;
    let result = page.evaluate_function(params).await.map_err(|e| closed(&e))?;
    // a JS null comes back as "no value"; hand it on as null so Option<T> works
    serde_json::from_value(result.value().cloned().unwrap_or(Value::Null)).map_err(|e| closed(&e))
}

async fn goto(page: &Page, url: &str) -> ToolResult<()> {
    page.goto(url).await.map(|_| ()).map_err(|e| format!("Chrome could not open Instagram ({e}). Is the bridge's Chrome window still open?"))
}

async fn find_reels(page: &Page, args: &Value) -> ToolResult<Value> {
    let topic = args["topic"].as_str().unwrap_or_default();
    let limit = args["limit"].as_u64().unwrap_or(10) as usize;
    let url = reqwest::Url::parse_with_params(&format!("{IG}explore/search/keyword/"), &[("q", topic)]).map_err(|e| e.to_string())?;
    goto(page, url.as_str()).await?;
    tokio::time::sleep(Duration::from_secs(3)).await; // let the results grid load
    let bodies: Option<Vec<String>> = call_js(page, SEARCH_JS, topic).await?;
    let reels = instagram::collect_reels(&bodies.ok_or(NOT_LOGGED_IN)?, limit);
    if reels.is_empty() {
        return Err(format!("No reels found for '{topic}'. Try a shorter or hashtag-style topic. If the Chrome window shows a login page: {NOT_LOGGED_IN}"));
    }
    serde_json::to_value(reels).map_err(|e| e.to_string())
}

async fn get_comments(page: &Page, args: &Value) -> ToolResult<Value> {
    let pk = instagram::shortcode_from_url(args["reel_url"].as_str().unwrap_or_default())
        .and_then(instagram::shortcode_to_pk)
        .ok_or("reel_url must look like https://www.instagram.com/reel/<code>/")?;
    let contains: Vec<String> = serde_json::from_value(args["contains"].clone()).unwrap_or_default();
    let max = args["max_comments"].as_u64().unwrap_or(300) as usize;
    // fetch() only carries the login when the page is on instagram.com
    if !page.url().await.ok().flatten().is_some_and(|u| u.starts_with(IG)) {
        goto(page, IG).await?;
    }
    let contains = &contains;
    let fetch = |cursor: String| async move {
        let body = call_js::<String>(page, FETCH_JS, &instagram::comments_url(pk, &cursor)?).await?;
        instagram::parse_comments(&body, contains)
    };
    let out = instagram::page_comments(fetch, max, args["cursor"].as_str().unwrap_or_default(), Duration::from_secs(1)).await?;
    serde_json::to_value(out).map_err(|e| e.to_string())
}

async fn run_job(page: &Page, job: &Job) -> ToolResult<Value> {
    match job.tool.as_str() {
        "find_reels" => find_reels(page, &job.args).await,
        "get_comments" => get_comments(page, &job.args).await,
        other => Err(format!("this bridge does not know the tool '{other}'; update it")),
    }
}

pub async fn run(s: Settings) -> ToolResult<()> {
    std::fs::create_dir_all(&s.profile).map_err(|e| format!("cannot create {}: {e}", s.profile.display()))?;
    let mut cfg = BrowserConfig::builder()
        .with_head() // a visible, normal Chrome: the user logs in here and Instagram sees a real browser
        .user_data_dir(&s.profile)
        .viewport(None)
        .arg("--disable-blink-features=AutomationControlled");
    if let Some(chrome) = &s.chrome {
        cfg = cfg.chrome_executable(chrome);
    }
    let (mut browser, mut events) = Browser::launch(cfg.build()?).await.map_err(|e| match e.to_string() {
        m if m.contains("ProcessSingleton") => "A bridge Chrome window is already open. Close it, then start the bridge again.".to_string(),
        m => format!("cannot start Chrome ({m}). Set CHROME to the browser's path."),
    })?;
    let chrome_gone = tokio::spawn(async move { while events.next().await.is_some() {} });
    let page = Arc::new(tokio::sync::Mutex::new(browser.new_page(IG).await.map_err(|e| format!("cannot open Instagram: {e}"))?));
    let http = reqwest::Client::builder().timeout(Duration::from_secs(40)).build().map_err(|e| e.to_string())?;
    eprintln!("Bridge connected to {}. Log in to Instagram in the Chrome window if it asks, and leave the window open.", s.server);

    let poll = async {
        loop {
            let res = http.get(format!("{}/bridge/next", s.server)).bearer_auth(&s.token).send().await;
            let job: Job = match res {
                Ok(r) if r.status() == StatusCode::UNAUTHORIZED => return Err("the server rejected MCP_TOKEN".to_string()),
                Ok(r) if r.status() == StatusCode::OK => match r.json().await {
                    Ok(job) => job,
                    Err(e) => {
                        eprintln!("unreadable job: {e}");
                        continue;
                    }
                },
                Ok(r) if r.status() == StatusCode::NO_CONTENT => continue,
                other => {
                    eprintln!("server not reachable ({}), retrying in 5s", other.map(|r| r.status().to_string()).unwrap_or_else(|e| e.to_string()));
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    continue;
                }
            };
            // answer in the background so polling never stops: the server treats a silent bridge as offline
            let (page, http, server, token) = (page.clone(), http.clone(), s.server.clone(), s.token.clone());
            tokio::spawn(async move {
                eprintln!("job {}: {}", job.id, job.tool);
                let result = {
                    let page = page.lock().await; // one Instagram job at a time, like a person
                    run_job(&page, &job).await
                }; // lock released before the upload, so a slow server never stalls the next job
                let body = match result {
                    Ok(v) => json!({"ok": v}),
                    Err(e) => json!({"err": e}),
                };
                let sent = http.post(format!("{server}/bridge/result/{}", job.id)).bearer_auth(token).json(&body).send().await;
                if let Err(e) = sent.and_then(|r| r.error_for_status()) {
                    eprintln!("job {}: could not send the result: {e}", job.id);
                }
            });
        }
    };
    let outcome = tokio::select! {
        r = poll => r,
        _ = chrome_gone => return Err("Chrome was closed. Start the bridge again.".into()),
        _ = tokio::signal::ctrl_c() => Ok(()),
    };
    // close Chrome ourselves: a leftover window keeps the profile locked and blocks the next start
    let _ = browser.close().await;
    let _ = browser.wait().await;
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |k| pairs.iter().find(|(n, _)| *n == k).map(|(_, v)| v.to_string())
    }

    #[test]
    fn settings() {
        let s = Settings::from_vars(vars(&[("SERVER_URL", "https://x.up.railway.app/"), ("MCP_TOKEN", "t"), ("HOME", "/home/u")])).ok().unwrap();
        assert_eq!((s.server.as_str(), s.profile.to_str().unwrap()), ("https://x.up.railway.app", "/home/u/.local/share/content-rd/chrome"));
        assert!(Settings::from_vars(vars(&[("MCP_TOKEN", "t"), ("HOME", "/h")])).err().unwrap().contains("SERVER_URL"));
        assert!(Settings::from_vars(vars(&[("SERVER_URL", "https://x"), ("HOME", "/h")])).err().unwrap().contains("MCP_TOKEN"));
        // a token must never travel over plain http to a remote host
        assert!(Settings::from_vars(vars(&[("SERVER_URL", "http://x.up.railway.app"), ("MCP_TOKEN", "t"), ("HOME", "/h")])).err().unwrap().contains("https"));
        assert!(Settings::from_vars(vars(&[("SERVER_URL", "http://localhost:8787"), ("MCP_TOKEN", "t"), ("HOME", "/h")])).is_ok());
        for sneaky in ["http://localhost.evil.com", "http://127.0.0.1.evil.com:80/x", "http://localhostx"] {
            let r = Settings::from_vars(move |k| match k {
                "SERVER_URL" => Some(sneaky.into()),
                "MCP_TOKEN" => Some("t".into()),
                "HOME" => Some("/h".into()),
                _ => None,
            });
            assert!(r.is_err(), "{sneaky}");
        }
    }

    #[test]
    fn finds_chrome_on_path() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("chromium"), "").unwrap();
        let path = format!("/nonexistent-dir:{}", dir.path().display());
        assert_eq!(find_chrome(&path), Some(dir.path().join("chromium")));
        assert_eq!(find_chrome("/nonexistent-dir"), None);
        let s = Settings::from_vars(|k| match k {
            "SERVER_URL" => Some("https://x".into()),
            "MCP_TOKEN" => Some("t".into()),
            "HOME" => Some("/h".into()),
            "PATH" => Some(path.clone()),
            _ => None,
        });
        assert_eq!(s.ok().unwrap().chrome, Some(dir.path().join("chromium")));
    }
}
