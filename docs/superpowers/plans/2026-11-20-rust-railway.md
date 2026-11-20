# Content R&D MCP: Rust Rebuild + Railway Deploy Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the local Python MCP server with a Rust binary hosted on Railway, same 7 tools and board page, reachable by any agent at a public URL.

**Architecture:** One Rust binary (axum + the official `rmcp` SDK, streamable HTTP) serves `/mcp` behind a bearer token and public read-only boards at `/b/<name>`. No browser: Instagram is read over plain HTTPS with a session cookie from an env var, and search results come from a private SearXNG service on the same Railway project. Boards are JSON files on a Railway volume.

**Tech Stack:** Rust stable, tokio, axum, rmcp (official MCP Rust SDK), reqwest (rustls), serde/serde_json, maud (auto-escaping HTML), subtle. SearXNG (open source) as a second Railway service. Docker, Railway.

**Spec:** `server.py` and `README.md` in the repo root are the behavior reference. Tool names, parameters, return shapes, the `WORKFLOW` text and the board JSON format carry over unless this plan says otherwise.

## Decisions that differ from the Python version

| Python | Rust on Railway | Why |
| --- | --- | --- |
| Chrome via Playwright | No browser, `reqwest` only | Railway has no display; Google captchas headless Chrome |
| `server.py login` window | `IG_SESSIONID` env var (cookie copied from the user's browser) | No interactive login on a server |
| `google_serp` scraping Google | `serp` tool calling SearXNG JSON API | Works without a browser; result lists which engines answered |
| `/` lists all boards | `/` returns `ok` | Public host must not enumerate boards |
| No auth, localhost | `MCP_TOKEN` bearer required on `/mcp` | Public host |
| `boards/` in repo dir | `DATA_DIR` on a Railway volume | Container disk is wiped on redeploy |

## Global Constraints

- Tool names: `find_reels`, `get_comments`, `keyword_ideas`, `keyword_interest`, `serp`, `board_add_comments`, `board_add_gap`. Prompt: `content_rd(industry, market)`.
- Board JSON stays `{"comments":[{pattern,user,text,likes,reel_url}],"gaps":[{keyword,interest,scores,verdict,results:[{title,url}]}]}` so `boards/demo-skincare.json` loads unchanged.
- Board name rule: `[a-z0-9][a-z0-9-]{0,63}`, enforced in every tool and in the GET route.
- Every tool failure returns an MCP tool error with a human-readable message. No `unwrap`/`expect` on network or user data.
- Env: `PORT` (default 8787), `MCP_TOKEN` (required, server exits without it), `DATA_DIR` (default `./boards`), `BASE_URL` (default `https://$RAILWAY_PUBLIC_DOMAIN`, else `http://localhost:$PORT`), `IG_SESSIONID`, `IG_PROXY`, `SEARXNG_URL` (all three optional; the tools that need a missing one return an error naming the variable).
- Bind `0.0.0.0:$PORT`.
- Instagram requests send `X-IG-App-ID: 936619743392459`, a desktop Chrome `User-Agent`, cookie `sessionid=<IG_SESSIONID>`, and wait 1 second between pages.
- `rmcp` API: do not write it from memory. Look up the current server, tool-router and streamable-HTTP-with-axum examples through context7 (`rmcp` / `modelcontextprotocol/rust-sdk`) and use the latest release.
- All secrets go through Railway variables. None in git, logs or fixtures.
- Shared error type everywhere: `pub type ToolResult<T> = Result<T, String>;` defined in `src/main.rs`.

## Review Focus

1. Instagram session expired or challenged (HTML login page, `{"message":"login_required"}`, `checkpoint_required`): tool error telling the user to refresh `IG_SESSIONID`, never a panic or an empty success. Test in Task 3.
2. Hostile comment text or links on the public board (`<script>`, `javascript:` URLs, quotes in attributes): rendered inert. Test in Task 2.
3. Board names like `../x`, `A`, empty, 65 characters, in tools and in the URL: rejected, no file touched. Test in Task 2 and Task 5.
4. `/mcp` without or with a wrong token: 401; `/b/<name>` stays readable without a token. Test in Task 5.
5. Google Trends edge cases (anchor with zero interest, empty timeline, more than 5 keywords, duplicate keywords, HTTP 429): clear error or correct batching, no divide-by-zero. Test in Task 4.

---

## File Structure

| File | Responsibility |
| --- | --- |
| `Cargo.toml` | dependencies |
| `src/main.rs` | `Config`, `ToolResult`, router, bearer check, startup |
| `src/board.rs` | board types, `Store` (file IO), HTML render |
| `src/instagram.rs` | shortcode math, JSON parsing, `Ig` HTTP client |
| `src/search.rs` | autocomplete, Trends, SearXNG |
| `src/tools.rs` | rmcp server: 7 tools, prompt, `WORKFLOW` instructions |
| `tests/fixtures/*.json` | captured real responses |
| `Dockerfile` | app image |
| `searxng/Dockerfile`, `searxng/settings.yml` | SearXNG with JSON output enabled |

Unit tests live in `#[cfg(test)] mod tests` at the bottom of each module. Fetching and parsing are separate functions so parsing is tested on fixtures with no network.

---

### Task 1: Prove the Instagram HTTP path and capture fixtures

This is a gate. The Python version never ran against a real session, and this design drops the browser. If the endpoints do not answer over plain HTTPS, stop and report before writing Rust.

**Files:**
- Create: `.gitignore`, `tests/fixtures/ig_comments.json`, `tests/fixtures/ig_search.json`, `tests/fixtures/ig_login_required.json`

**Interfaces:**
- Produces: the three fixtures, and a one-line note at the top of this task (edit the plan) saying which search endpoint worked.

- [ ] **Step 1:** `git init`; write `.gitignore` with `.venv/ chrome-profile/ target/ boards/ server.log *.png .env`.
- [ ] **Step 2:** Ask the user for the `sessionid` cookie of a secondary Instagram account with `request_secret` (`delivery: "file"`). Never echo it.
- [ ] **Step 3:** Fetch comments for any public reel with over 1000 comments:
  `curl -s -H 'X-IG-App-ID: 936619743392459' -A '<desktop Chrome UA>' -b "sessionid=$(cat <file>)" 'https://www.instagram.com/api/v1/media/<pk>/comments/?can_support_threading=true&permalink_enabled=false'`
  Expected: JSON with a `comments` array and `next_min_id`. Repeat once with `&min_id=<urlencoded next_min_id>` and confirm different comments.
- [ ] **Step 4:** Try search in this order and keep the first that returns objects holding both `code` and `comment_count`: `/api/v1/fbsearch/web/top_serp/?query=skincare`, then `/api/v1/tags/web_info/?tag_name=skincare`.
- [ ] **Step 5:** Call the comments URL with no cookie; save the body as `ig_login_required.json` (if the body is HTML, save `{"_html": true}` and note it).
- [ ] **Step 6:** Save the two good responses as fixtures. Replace usernames with `user1..n` and delete every key containing `pic`, `url`, `token` or `id` except `next_min_id`, `code`, `pk`.
- [ ] **Step 7:** Commit: `chore: repo init and Instagram fixtures`.

**Stop condition:** comments return a login page or checkpoint even with a fresh cookie. Report to the user; the fallback is keeping a browser, which needs a different host than this plan assumes.

---

### Task 2: Board module

**Files:**
- Create: `Cargo.toml`, `src/main.rs` (only `mod board;`, `ToolResult`, empty `main`), `src/board.rs`

**Interfaces:**
- Produces:
  - `struct Card { pattern, user, text: String, likes: u64, reel_url: String }`
  - `struct Link { title, url: String }`
  - `struct Gap { keyword: String, interest: i64, scores: BTreeMap<String, i64>, verdict: String, results: Vec<Link> }`
  - `struct Board { comments: Vec<Card>, gaps: Vec<Gap> }` (all `Serialize + Deserialize + Default`, missing fields default)
  - `fn valid_name(name: &str) -> bool`
  - `impl Board { fn add_comments(&mut self, pattern: &str, cards: Vec<Card>); fn upsert_gap(&mut self, gap: Gap) }`
  - `struct Store { dir: PathBuf, lock: tokio::sync::Mutex<()> }` with `fn new(dir) -> Store`, `async fn update(&self, name: &str, f: impl FnOnce(&mut Board)) -> ToolResult<()>`, `fn load(&self, name: &str) -> ToolResult<Option<Board>>`
  - `fn render(name: &str, board: &Board) -> maud::Markup`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn names() {
    for ok in ["a", "skincare-eg", "x9"] { assert!(valid_name(ok)); }
    for bad in ["", "A", "../x", "a/b", "-a", "a b", &"a".repeat(65)] { assert!(!valid_name(bad)); }
}
#[test] fn dedupes_comments() {
    let mut b = Board::default();
    let c = || Card { pattern: "p".into(), user: "u".into(), text: "t".into(), likes: 1, reel_url: "".into() };
    b.add_comments("p", vec![c(), c()]); b.add_comments("p", vec![c()]);
    assert_eq!(b.comments.len(), 1);
}
#[test] fn upsert_replaces_same_keyword() { /* two gaps "k" -> len 1, second one's interest kept */ }
#[test] fn render_is_inert_and_ranked() {
    // comment text "<script>alert(1)</script>", pattern "<b>p", reel_url "javascript:alert(1)",
    // user "\" onmouseover=\"x", gaps kw-b (scores sum 1) then kw-a (sum 5)
    let html = render("t", &board).into_string();
    assert!(!html.contains("<script>alert") && !html.contains("javascript:") && !html.contains("<b>p"));
    assert!(!html.contains("\" onmouseover"));
    assert!(html.find("kw-a").unwrap() < html.find("kw-b").unwrap());
}
#[tokio::test] async fn store_roundtrip_and_rejects_bad_names() {
    // tempdir; update("demo", add one card) then load("demo") has 1 comment;
    // update("../x", ..) is Err and the tempdir parent has no new file; load("nope") == Ok(None)
}
#[test] fn loads_python_board() { /* serde_json::from_str(include_str!("../boards/demo-skincare.json")) is Ok */ }
```

Copy `boards/demo-skincare.json` to `tests/fixtures/board_python.json` and point the last test there, since `boards/` is ignored.

- [ ] **Step 2:** Run `cargo test board` and confirm it fails to compile on the missing items.
- [ ] **Step 3: Implement `src/board.rs`.** `Store::update` takes the lock, loads or defaults, applies `f`, writes `<name>.json.tmp` then renames (a crash must not truncate a board). `render` ports the layout and `CSS` constant from `server.py` (`render`, `_link`, `CSS`): gaps table sorted by score sum descending, comments grouped by pattern (largest group first, cards by likes descending), `dir="auto"` on user text, links only when the URL starts with `https://`, otherwise plain text. Use maud's default escaping; no `PreEscaped` on user data. Drop the index page.
- [ ] **Step 4:** Run `cargo test board`. Expected: 6 passed.
- [ ] **Step 5:** Commit: `feat: board storage and page`.

---

### Task 3: Instagram module

**Files:**
- Create: `src/instagram.rs`
- Modify: `src/main.rs` (add `mod instagram;`)

**Interfaces:**
- Consumes: `ToolResult`, fixtures from Task 1.
- Produces:
  - `struct Reel { url: String, plays: u64, likes: u64, comments: u64, user: String, caption: String }` (`Serialize`)
  - `struct Comment { user: String, text: String, likes: u64, replies: u64 }` (`Serialize`)
  - `struct CommentPage { comments: Vec<Comment>, scanned: usize, next_cursor: String }` (`Serialize`)
  - `fn shortcode_from_url(url: &str) -> Option<&str>`
  - `fn shortcode_to_pk(code: &str) -> Option<u64>`
  - `fn walk_media(v: &Value, out: &mut HashMap<String, Reel>)`
  - `fn parse_comments(body: &str, contains: &[String]) -> ToolResult<(Vec<Comment>, usize, String)>` (kept, scanned, next_min_id)
  - `struct Ig` with `fn new(sessionid: Option<&str>, proxy: Option<&str>) -> ToolResult<Ig>`, `async fn find_reels(&self, topic: &str, limit: usize) -> ToolResult<Vec<Reel>>`, `async fn get_comments(&self, reel_url: &str, max_comments: usize, cursor: &str, contains: &[String]) -> ToolResult<CommentPage>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn shortcodes() {
    assert_eq!(shortcode_to_pk("BA"), Some(64));
    assert_eq!(shortcode_to_pk("_"), Some(63));
    assert_eq!(shortcode_to_pk("a b"), None);               // bad character
    assert_eq!(shortcode_to_pk(&"_".repeat(40)), None);      // private-post codes overflow u64
    assert_eq!(shortcode_from_url("https://www.instagram.com/reel/DAbc-_1/"), Some("DAbc-_1"));
    assert_eq!(shortcode_from_url("https://www.instagram.com/someone/p/Cxyz/?igsh=1"), Some("Cxyz"));
    assert_eq!(shortcode_from_url("https://example.com/reel/x"), None);
}
#[test] fn walks_fixture() {
    let mut out = HashMap::new();
    walk_media(&serde_json::from_str(include_str!("../tests/fixtures/ig_search.json")).unwrap(), &mut out);
    assert!(!out.is_empty());
    assert!(out.values().all(|r| r.url.starts_with("https://www.instagram.com/reel/")));
}
#[test] fn parses_comments_fixture() {
    let (kept, scanned, next) = parse_comments(include_str!("../tests/fixtures/ig_comments.json"), &[]).unwrap();
    assert!(scanned > 0 && kept.len() <= scanned && !next.is_empty());
    assert!(kept.iter().all(|c| c.text.split_whitespace().count() >= 3));
}
#[test] fn contains_filters_case_insensitive() { /* inline JSON, 2 comments, contains ["PRICE"] keeps the one with "price" */ }
#[test] fn session_errors_are_messages() {
    for body in [include_str!("../tests/fixtures/ig_login_required.json"), "<!DOCTYPE html><html>login</html>",
                 r#"{"message":"checkpoint_required","status":"fail"}"#] {
        assert!(parse_comments(body, &[]).unwrap_err().contains("IG_SESSIONID"));
    }
}
```

- [ ] **Step 2:** Run `cargo test instagram` and confirm it fails.
- [ ] **Step 3: Implement.** Port `shortcode_to_pk`, `walk_media` and the comment loop from `server.py` (same field fallbacks: `play_count`/`ig_play_count`/`view_count`, `user`/`owner`, caption as object or string, cut to 160 chars on a char boundary). `shortcode_to_pk` uses checked arithmetic. `parse_comments` treats non-JSON, `status != "ok"`, or a missing `comments` key with a `message` as a session error. `Ig::new(None, _)` succeeds; its methods then return `Err("IG_SESSIONID is not set …")`. `get_comments` loops pages until `scanned >= max_comments` or the cursor is empty, sleeping 1 s between pages. `find_reels` calls the endpoint(s) recorded in Task 1, sorts by `(plays, comments)` descending, errors when nothing is found.
- [ ] **Step 4:** Run `cargo test instagram`. Expected: 5 passed.
- [ ] **Step 5:** Commit: `feat: instagram reels and comments over http`.

---

### Task 4: Search module

**Files:**
- Create: `src/search.rs`, `tests/fixtures/searxng.json` (hand-written, 12 results, fields `title`, `url`, `content`, `engines`)
- Modify: `src/main.rs` (add `mod search;`)

**Interfaces:**
- Consumes: `ToolResult`.
- Produces:
  - `struct SerpResult { title, url, snippet: String, engines: Vec<String> }` (`Serialize`)
  - `fn strip_guard(text: &str) -> ToolResult<Value>` (drops the `)]}'` prefix)
  - `fn batches(keywords: &[String], anchor: &str) -> Vec<Vec<String>>`
  - `fn relative(means: &[f64]) -> Option<Vec<i64>>`
  - `fn parse_searxng(body: &Value) -> Vec<SerpResult>`
  - `async fn keyword_ideas(http: &Client, seeds: &[String], lang: &str, country: &str) -> ToolResult<BTreeMap<String, Vec<String>>>`
  - `async fn keyword_interest(http: &Client, keywords: &[String], geo: &str, anchor: &str) -> ToolResult<BTreeMap<String, i64>>`
  - `async fn serp(http: &Client, searxng_url: Option<&str>, keyword: &str, lang: &str, country: &str) -> ToolResult<Vec<SerpResult>>`

- [ ] **Step 1: Write the failing tests**

```rust
#[test] fn guard() {
    assert_eq!(strip_guard(")]}'\n{\"a\":1}").unwrap()["a"], 1);
    assert!(strip_guard("<html>429</html>").is_err());
}
#[test] fn batching() {
    let k: Vec<String> = ["a","b","c","d","e","f","b","a"].map(String::from).to_vec();
    let b = batches(&k, "a");
    assert_eq!(b, vec![vec!["a","b","c","d","e"], vec!["a","f"]]);   // anchor first, max 5, deduped
    assert_eq!(batches(&["x".to_string()], "anchor"), vec![vec!["anchor","x"]]);
}
#[test] fn relative_scores() {
    assert_eq!(relative(&[50.0, 25.0, 130.0, 0.0]), Some(vec![50, 260, 0]));  // anchor dropped, anchor = 100
    assert_eq!(relative(&[0.0, 10.0]), None);
    assert_eq!(relative(&[]), None);
}
#[test] fn searxng_top_ten() {
    let r = parse_searxng(&serde_json::from_str(include_str!("../tests/fixtures/searxng.json")).unwrap());
    assert_eq!(r.len(), 10);
    assert!(!r[0].engines.is_empty());
    assert!(parse_searxng(&serde_json::json!({})).is_empty());
}
```

- [ ] **Step 2:** Run `cargo test search` and confirm it fails.
- [ ] **Step 3: Implement.** Port `keyword_ideas` and `keyword_interest` from `server.py` (same URLs, params, cookie warm-up request, 1 s pause between batches; reqwest `cookie_store(true)`). Empty timeline counts as mean 0. `relative` returning `None` becomes the error `Anchor '<x>' has no search interest in <geo>; pick a more popular anchor.` Non-2xx becomes `Google Trends returned <status>; wait a minute and retry.` `serp` calls `GET {searxng_url}/search?q=&format=json&language={lang}-{COUNTRY}`, snippet from `content` cut to 300 chars; `None` URL returns `Err("SEARXNG_URL is not set")`.
- [ ] **Step 4:** Run `cargo test search`. Expected: 4 passed.
- [ ] **Step 5:** Live check, no secrets needed: add `#[tokio::test] #[ignore] async fn live_ideas_and_trends()` asserting `keyword_ideas(["skincare"])` is non-empty and `keyword_interest(["skincare","sunscreen"], "EG", "")` has both keys. Run `cargo test live -- --ignored`. Expected: 1 passed.
- [ ] **Step 6:** Commit: `feat: keyword ideas, trends and serp`.

---

### Task 5: MCP server, routes and auth

**Files:**
- Create: `src/tools.rs`
- Modify: `src/main.rs`

**Interfaces:**
- Consumes: everything produced by Tasks 2 to 4.
- Produces:
  - `struct Config { port: u16, token: String, data_dir: PathBuf, base_url: String, ig_sessionid: Option<String>, ig_proxy: Option<String>, searxng_url: Option<String> }`, `Config::from_env() -> Result<Config, String>`
  - `fn app(cfg: Config) -> axum::Router` (used by tests and `main`)
  - Routes: `GET /` → `ok`; `GET /b/{board}` → HTML or 404; `/mcp` → rmcp streamable HTTP service behind the bearer check.

- [ ] **Step 1: Write the failing tests** (in `src/main.rs`, using `tower::ServiceExt::oneshot` on `app(test_cfg(tempdir))`)

```rust
#[tokio::test] async fn mcp_requires_token() {
    // POST /mcp, no header            -> 401
    // POST /mcp, "Bearer wrong"       -> 401
    // POST /mcp, "Bearer <token>" with a JSON-RPC initialize body and
    //   Accept: application/json, text/event-stream -> 200
}
#[tokio::test] async fn boards_are_public_and_safe() {
    // GET /                -> 200 "ok"
    // GET /b/nope          -> 404
    // GET /b/..%2Fsecret   -> 404
    // after store.update("demo", add card): GET /b/demo -> 200, body contains the card text
}
#[test] fn config_needs_token() { /* from_env without MCP_TOKEN is Err mentioning MCP_TOKEN; empty string too */ }
```

- [ ] **Step 2:** Run `cargo test` and confirm the new tests fail.
- [ ] **Step 3: Implement `src/tools.rs`.** One rmcp server struct holding `Ig`, `reqwest::Client`, `Arc<Store>`, `base_url`, `searxng_url`. Seven tools with the parameter names, defaults and docstrings of `server.py` (`google_serp` becomes `serp`; its description says results come from SearXNG and the `engines` field shows which engines agreed). Board tools validate with `valid_name`, call `Store::update`, return `{base_url}/b/{board}`. `board_add_gap` keeps at most 10 results. `Err(String)` maps to an MCP tool error result carrying the message. Server instructions are the `WORKFLOW` text from `server.py` with step B3 reading `serp(keyword)`. Prompt `content_rd(industry, market)` returns the same text as the Python one.
- [ ] **Step 4: Implement `src/main.rs`.** Bearer check is an axum middleware on `/mcp` comparing with `subtle::ConstantTimeEq`. If rmcp restricts allowed hosts, allow the host from `base_url`. `main` prints `Config::from_env` errors and exits 1.
- [ ] **Step 5:** Run `cargo test`. Expected: all pass. Run `cargo clippy -- -D warnings`. Expected: clean.
- [ ] **Step 6:** Protocol check against a real client:
  `MCP_TOKEN=dev cargo run &` then
  `npx @modelcontextprotocol/inspector --cli http://localhost:8787/mcp --transport http --header "Authorization: Bearer dev" --method tools/list`
  Expected: the 7 tool names. Then `--method tools/call --tool-name board_add_gap --tool-arg board=../x …` returns an error containing `board name`.
- [ ] **Step 7:** Commit: `feat: mcp server with auth and board routes`.

---

### Task 6: Deploy to Railway and retire Python

Load the `use-railway` skill first. Creating services costs money on the user's Railway account; this task is the user-approved place for that.

**Files:**
- Create: `Dockerfile`, `.dockerignore`, `searxng/Dockerfile`, `searxng/settings.yml`
- Modify: `README.md` (rewrite for the hosted version)
- Delete: `server.py`, `test_server.py`, `.venv/`, `chrome-profile/`, `server.log`, `board-demo.png`

**Interfaces:**
- Consumes: `app`, env contract from Global Constraints.

- [ ] **Step 1:** `Dockerfile`: build stage `rust:1-slim` running `cargo build --release --locked`; run stage `debian:stable-slim` with `ca-certificates`, the binary, `CMD ["/app/content-rd"]`. Runs as root so the volume is writable. `.dockerignore`: `target .venv chrome-profile boards .git`.
- [ ] **Step 2:** `searxng/settings.yml`:

```yaml
use_default_settings: true
server:
  limiter: false          # private network only, never give this service a public domain
search:
  formats: [html, json]   # json is off by default and serp needs it
```

  `searxng/Dockerfile`: `FROM searxng/searxng:latest` plus `COPY settings.yml /etc/searxng/settings.yml`.
- [ ] **Step 3:** Local image check: `docker build -t content-rd . && docker run --rm -e MCP_TOKEN=dev -p 8787:8787 content-rd`, then `curl localhost:8787/` prints `ok`.
- [ ] **Step 4:** Railway: create project `content-rd` with two services.
  - `searxng`: root directory `searxng/`, variable `SEARXNG_SECRET=<random 32 bytes>`, no public domain. If the app cannot reach it, set its bind address to `::` (Railway private networking).
  - `content-rd`: repo root, volume mounted at `/data`, generated public domain, variables `DATA_DIR=/data`, `MCP_TOKEN=<random 32 bytes>`, `SEARXNG_URL=http://searxng.railway.internal:8080`, `IG_SESSIONID` (from the user via `request_secret`), healthcheck path `/`.
- [ ] **Step 5:** Smoke test against the public URL, with `$U` the domain and `$T` the token:
  - `curl -s $U/` → `ok`
  - `curl -s -o /dev/null -w '%{http_code}' -X POST $U/mcp` → `401`
  - inspector `tools/list` with the bearer header → 7 tools
  - inspector `tools/call`: `serp` (`keyword=best sunscreen for oily skin`) returns results; `keyword_interest` returns numbers; `get_comments` on a public reel returns comments; `board_add_gap` on board `smoke` returns `$U/b/smoke`
  - `curl -s $U/b/smoke` → 200. Redeploy the service, curl again → still 200 (volume works).
- [ ] **Step 6:** Record each smoke result in the task notes. Any tool that fails from Railway's IP (Instagram checkpoint, Trends 429, SearXNG with no Google engine) is reported to the user with the exact error, not hidden. Known remedy for Instagram: set `IG_PROXY`.
- [ ] **Step 7:** Delete the Python files listed above. Rewrite `README.md`: what it is, the connect command `claude mcp add --transport http content-rd $U/mcp --header "Authorization: Bearer <token>"`, the one-line prompt, env table, how to refresh `IG_SESSIONID`, the tool table.
- [ ] **Step 8:** Commit: `feat: railway deploy, remove python server`. Give the user the public URL and where the token is stored (Railway variables).

---

## Known risks the user accepts by approving

- An Instagram cookie used from a datacenter IP can trigger a security checkpoint or lock the account. Use a secondary account. Task 1 and Task 6 both test this before anything depends on it.
- Google often blocks datacenter IPs, so SearXNG may answer from Bing, DuckDuckGo or Brave instead of Google. The `engines` field shows which.
- Google Trends rate-limits datacenter IPs more than home IPs.
- Board pages are public to anyone who has or guesses the URL. They contain public commenters' usernames.
