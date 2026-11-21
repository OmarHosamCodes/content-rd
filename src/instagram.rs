//! Instagram: parsing for both paths, plus the logged-out public GraphQL calls the server makes itself.
//! The logged-in browser path (fallback) lives in bridge.rs.
use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::ToolResult;

#[derive(Serialize, Debug, Clone)]
pub struct Reel {
    pub url: String,
    pub plays: u64,
    /// not in public search results
    #[serde(skip_serializing_if = "Option::is_none")]
    pub likes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comments: Option<u64>,
    pub user: String,
    pub caption: String,
}

#[derive(Serialize, Debug)]
pub struct Comment {
    pub user: String,
    pub text: String,
    pub likes: u64,
    pub replies: u64,
}

fn code_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '-' || c == '_'
}

pub fn shortcode_from_url(url: &str) -> Option<&str> {
    let path = url.split_once("instagram.com/")?.1;
    let path = path.split(['?', '#']).next()?;
    let mut parts = path.split('/');
    while let Some(part) = parts.next() {
        if matches!(part, "reel" | "reels" | "p" | "tv") {
            return parts.next().filter(|c| !c.is_empty() && c.chars().all(code_char));
        }
    }
    None
}

/// Instagram shortcodes are the media id in base 64.
pub fn shortcode_to_pk(code: &str) -> Option<u64> {
    const ABC: &str = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    if code.is_empty() {
        return None;
    }
    code.chars().try_fold(0u64, |n, c| n.checked_mul(64)?.checked_add(ABC.find(c)? as u64))
}

/// Collect every post-like object from any Instagram JSON, whatever its shape.
fn walk_media(v: &Value, out: &mut HashMap<String, Reel>) {
    match v {
        Value::Object(o) => {
            if let (Some(code), true) = (o.get("code").and_then(Value::as_str), o.contains_key("comment_count")) {
                let n = |keys: &[&str]| keys.iter().find_map(|k| o.get(*k).and_then(Value::as_u64)).unwrap_or(0);
                let caption = match o.get("caption") {
                    Some(Value::String(s)) => s.as_str(),
                    Some(c) => c.get("text").and_then(Value::as_str).unwrap_or(""),
                    None => "",
                };
                let user = ["user", "owner"].iter().find_map(|k| o.get(*k)?.get("username")?.as_str()).unwrap_or("");
                out.insert(code.to_string(), Reel {
                    url: format!("https://www.instagram.com/reel/{code}/"),
                    plays: n(&["play_count", "ig_play_count", "view_count"]),
                    likes: Some(n(&["like_count"])),
                    comments: Some(n(&["comment_count"])),
                    user: user.to_string(),
                    caption: caption.chars().take(160).collect(),
                });
            }
            o.values().for_each(|v| walk_media(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| walk_media(v, out)),
        _ => {}
    }
}

/// Top reels found in any of the raw response bodies, by plays then comment count. Non-JSON bodies are skipped.
pub fn collect_reels(bodies: &[String], limit: usize) -> Vec<Reel> {
    let mut found = HashMap::new();
    for body in bodies {
        // some endpoints prefix JSON with `for (;;);`
        if let Some(v) = body.find(['{', '[']).and_then(|i| serde_json::from_str::<Value>(&body[i..]).ok()) {
            walk_media(&v, &mut found);
        }
    }
    let mut reels: Vec<Reel> = found.into_values().collect();
    reels.sort_by_key(|r| std::cmp::Reverse((r.plays, r.comments)));
    reels.truncate(limit);
    reels
}

/// The logged-in comments API (bridge) URL for one page.
pub fn comments_url(pk: u64, cursor: &str) -> ToolResult<String> {
    let mut params = vec![("can_support_threading", "true"), ("permalink_enabled", "false")];
    if !cursor.is_empty() {
        params.push(("min_id", cursor));
    }
    Ok(reqwest::Url::parse_with_params(&format!("https://www.instagram.com/api/v1/media/{pk}/comments/"), &params).map_err(|e| e.to_string())?.into())
}

/// One page of the logged-in comments API.
pub fn parse_comments(body: &str, contains: &[String]) -> ToolResult<Page> {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let Some(list) = v.get("comments").and_then(Value::as_array) else {
        return Err(match v.get("message").and_then(Value::as_str) {
            // anything else (rate limits, deleted reels) is not fixed by logging in again
            Some(said) if !said.contains("login") && !said.contains("checkpoint") => format!("Instagram refused: {said}"),
            said => format!("Log in to Instagram in the bridge's Chrome window, then retry. (Instagram answered: {})", said.unwrap_or("a login page")),
        });
    };
    let next = v.get("next_min_id").and_then(Value::as_str).unwrap_or("").to_string();
    Ok((keep_comments(list, contains), list.len(), next))
}

/// Comments under 3 words are noise. `contains` keeps only comments holding any of the words.
/// Both APIs name the fields the same way.
fn keep_comments(list: &[Value], contains: &[String]) -> Vec<Comment> {
    let needles: Vec<String> = contains.iter().map(|k| k.to_lowercase()).collect();
    list.iter()
        .filter_map(|c| {
            let text = c.get("text")?.as_str()?;
            let lower = text.to_lowercase();
            (text.split_whitespace().count() >= 3 && (needles.is_empty() || needles.iter().any(|k| lower.contains(k)))).then(|| Comment {
                user: c.pointer("/user/username").and_then(Value::as_str).unwrap_or("").to_string(),
                text: text.to_string(),
                likes: c.get("comment_like_count").and_then(Value::as_u64).unwrap_or(0),
                replies: c.get("child_comment_count").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect()
}

#[derive(Serialize, Debug)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
    pub scanned: usize,
    pub next_cursor: String,
}

/// One parsed page: (kept comments, scanned count, next cursor; empty on the last page).
pub type Page = (Vec<Comment>, usize, String);

/// Scroll a reel's comments from `cursor` until `max` are scanned or Instagram runs out.
/// `fetch` gets a cursor ("" = first page) and returns that page, from the public API or the bridge's Chrome.
pub async fn page_comments<F, Fut>(mut fetch: F, max: usize, cursor: &str, pause: std::time::Duration) -> ToolResult<CommentPage>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ToolResult<Page>>,
{
    let mut out = CommentPage { comments: Vec::new(), scanned: 0, next_cursor: cursor.to_string() };
    loop {
        let (kept, scanned, next) = match fetch(out.next_cursor.clone()).await {
            Ok(page) => page,
            // keep what was already read; next_cursor still points at the page that failed, so the caller can resume
            Err(_) if out.scanned > 0 => return Ok(out),
            Err(e) => return Err(e),
        };
        out.comments.extend(kept);
        out.scanned += scanned;
        out.next_cursor = next;
        if scanned == 0 || out.next_cursor.is_empty() || out.scanned >= max {
            return Ok(out);
        }
        tokio::time::sleep(pause).await; // stay gentle: fast paging gets accounts flagged
    }
}


// ---- public (logged-out) GraphQL, what instagram.com serves to visitors without an account ----

const IG_UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
// shortcut: persisted query ids copied from instagram.com's JS on 2026-09-22 (via omkarcloud/instagram-scraper); they rotate,
// and a stale one fails with "doc_id may have rotated". Re-harvest from the www bundles (`_instagramRelayOperation`) then.
const SEARCH_QUERY: (&str, &str) = ("PolarisLoggedOutPopularSearchPageQuery", "29032174876406682");
const COMMENTS_QUERY: (&str, &str) = ("PolarisLoggedOutDesktopWWWPostCommentsPaginationQuery", "27659279553772821");

pub fn client() -> ToolResult<reqwest::Client> {
    // no cookie store: every call is a fresh anonymous visitor with its own csrf token
    reqwest::Client::builder().user_agent(IG_UA).timeout(std::time::Duration::from_secs(30)).build().map_err(|e| e.to_string())
}

/// Instagram does not check the csrf/lsd pair, it only wants one; not a secret, so no crypto RNG needed.
fn token(len: usize) -> String {
    use std::hash::BuildHasher;
    let mut s = String::new();
    while s.len() < len {
        s += &format!("{:016x}", std::collections::hash_map::RandomState::new().hash_one(s.len()));
    }
    s.truncate(len);
    s
}

async fn graphql(http: &reqwest::Client, (name, doc_id): (&str, &str), variables: Value) -> ToolResult<String> {
    let (csrf, lsd) = (token(32), format!("AV{}", token(9)));
    let form = [
        ("av", "0"), ("__d", "www"), ("__user", "0"), ("__a", "1"), ("__comet_req", "7"), ("lsd", &lsd),
        ("fb_api_caller_class", "RelayModern"), ("fb_api_req_friendly_name", name),
        ("variables", &variables.to_string()), ("server_timestamps", "true"), ("doc_id", doc_id),
    ];
    // without the sec-fetch headers Instagram answers 200 with a "Your Request Couldn't be Processed" error
    let resp = http
        .post("https://www.instagram.com/api/graphql")
        .header("sec-fetch-dest", "empty")
        .header("sec-fetch-mode", "cors")
        .header("sec-fetch-site", "same-origin")
        .header("origin", "https://www.instagram.com")
        .header("referer", "https://www.instagram.com/")
        .header("x-ig-app-id", "936619743392459")
        .header("x-csrftoken", &csrf)
        .header("x-fb-lsd", &lsd)
        .header("cookie", format!("csrftoken={csrf}"))
        .form(&form)
        .send()
        .await
        .map_err(|e| format!("Instagram did not answer: {e}"))?;
    let status = resp.status().as_u16();
    let body = resp.text().await.map_err(|e| format!("Instagram did not answer: {e}"))?;
    match status {
        200 | 400 => Ok(body), // 400 carries a JSON error the parsers explain
        429 => Err("Instagram is rate-limiting this server (HTTP 429). Wait a few minutes, then retry.".into()),
        s => Err(format!("Instagram refused the logged-out request (HTTP {s}): {}", body.chars().take(120).collect::<String>())),
    }
}

/// The `data` object of a GraphQL answer, or why there is none.
fn gql_data(body: &str, name: &str) -> ToolResult<Value> {
    let Some(v) = body.find('{').and_then(|i| serde_json::from_str::<Value>(&body[i..]).ok()) else {
        return Err("Instagram sent a page instead of data; it may be blocking this server. Retry later.".into());
    };
    if let Some(data) = v.get("data").filter(|d| d.is_object()) {
        return Ok(data.clone());
    }
    let said = v.pointer("/errors/0/message").or_else(|| v.get("errorSummary")).or_else(|| v.get("message")).and_then(Value::as_str).unwrap_or("no data");
    if said.contains("execution error") || said.contains("missing_required") {
        return Err(format!("Instagram's public query {name} failed ({said}); its doc_id may have rotated."));
    }
    Err(format!("Instagram refused: {said}"))
}

pub fn parse_public_reels(body: &str, limit: usize) -> ToolResult<Vec<Reel>> {
    let data = gql_data(body, SEARCH_QUERY.0)?;
    let edges = data.pointer("/xig_logged_out_popular_search_media_info/edges").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut reels: Vec<Reel> = edges
        .iter()
        .filter_map(|e| {
            let n = e.get("node")?;
            Some(Reel {
                url: format!("https://www.instagram.com/reel/{}/", n.get("code")?.as_str()?),
                plays: n.get("play_count").and_then(Value::as_u64).unwrap_or(0),
                likes: None,
                comments: None,
                user: n.pointer("/user/username").and_then(Value::as_str).unwrap_or("").to_string(),
                caption: n.pointer("/caption/text").and_then(Value::as_str).unwrap_or("").chars().take(160).collect(),
            })
        })
        .collect();
    reels.sort_by_key(|r| std::cmp::Reverse(r.plays));
    reels.truncate(limit);
    Ok(reels)
}

pub fn parse_public_comments(body: &str, contains: &[String]) -> ToolResult<Page> {
    let data = gql_data(body, COMMENTS_QUERY.0)?;
    let conn = data.pointer("/xig_polaris_media/comments_connection").ok_or("Reel not found, or it is not public.")?;
    let nodes: Vec<Value> = conn.get("edges").and_then(Value::as_array).into_iter().flatten().filter_map(|e| e.get("node").cloned()).collect();
    let more = conn.pointer("/page_info/has_next_page").and_then(Value::as_bool).unwrap_or(false);
    let next = conn.pointer("/page_info/end_cursor").and_then(Value::as_str).filter(|_| more).unwrap_or("").to_string();
    Ok((keep_comments(&nodes, contains), nodes.len(), next))
}

/// Top public reels for a topic: one page of instagram.com/popular/<topic>/, most plays first.
pub async fn public_reels(http: &reqwest::Client, topic: &str, limit: usize) -> ToolResult<Vec<Reel>> {
    let vars = serde_json::json!({"keyword": topic, "media_count": 12, "related_keywords_count": 10, "include_reel_experience_fields": true,
        "include_description": false, "include_trending_pills": false, "debug": null, "after": null});
    // shortcut: one page (12 reels) per topic; add the end_cursor loop if agents need more per topic
    let reels = parse_public_reels(&graphql(http, SEARCH_QUERY, vars).await?, limit)?;
    if reels.is_empty() {
        return Err(format!("No public reels for '{topic}'. Instagram's public search only knows some phrasings: try a more common or two-word form (\"skin care\", not \"skincare\")."));
    }
    Ok(reels)
}

pub async fn public_comments(http: &reqwest::Client, pk: u64, max: usize, cursor: &str, contains: &[String]) -> ToolResult<CommentPage> {
    let fetch = |cursor: String| async move {
        let vars = serde_json::json!({"media_id": pk.to_string(), "first": 24, "after": (!cursor.is_empty()).then_some(cursor)});
        parse_public_comments(&graphql(http, COMMENTS_QUERY, vars).await?, contains)
    };
    page_comments(fetch, max, cursor, std::time::Duration::from_secs(1)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn shortcodes() {
        assert_eq!(shortcode_to_pk("BA"), Some(64));
        assert_eq!(shortcode_to_pk("_"), Some(63));
        assert_eq!(shortcode_to_pk("a b"), None);
        assert_eq!(shortcode_to_pk(""), None);
        assert_eq!(shortcode_to_pk(&"_".repeat(40)), None); // private-post codes overflow u64
        assert_eq!(shortcode_from_url("https://www.instagram.com/reel/DAbc-_1/"), Some("DAbc-_1"));
        assert_eq!(shortcode_from_url("https://www.instagram.com/someone/p/Cxyz/?igsh=1"), Some("Cxyz"));
        assert_eq!(shortcode_from_url("https://instagram.com/reels/Cq1"), Some("Cq1"));
        assert_eq!(shortcode_from_url("https://example.com/reel/x"), None);
        assert_eq!(shortcode_from_url("https://www.instagram.com/someone/"), None);
    }

    #[test]
    fn walks_fixture() {
        let reels = collect_reels(&[include_str!("../tests/fixtures/ig_search.json").to_string(), "not json".into()], 2);
        assert_eq!(reels.len(), 2);
        assert_eq!(reels[0].url, "https://www.instagram.com/reel/Cbbb222/"); // most plays first
        assert_eq!((reels[0].plays, reels[0].user.as_str(), reels[0].caption.as_str()), (900000, "user2", ""));
        assert_eq!((reels[1].comments, reels[1].caption.as_str()), (Some(900), "my routine"));
    }

    #[test]
    fn long_captions_are_cut_on_a_char_boundary() {
        let body = serde_json::json!({"code": "C1", "comment_count": 1, "caption": "ع".repeat(500)}).to_string();
        assert_eq!(collect_reels(&[body], 5)[0].caption.chars().count(), 160);
    }

    #[test]
    fn parses_comments_fixture() {
        let (kept, scanned, next) = parse_comments(include_str!("../tests/fixtures/ig_comments.json"), &[]).unwrap();
        assert_eq!((kept.len(), scanned), (2, 4)); // emoji-only and null comments dropped
        assert!(next.contains("server_cursor"));
        assert_eq!((kept[0].user.as_str(), kept[0].likes, kept[0].replies), ("user1", 12, 2));
    }

    #[test]
    fn contains_filters_case_insensitive() {
        let f = include_str!("../tests/fixtures/ig_comments.json");
        let (kept, _, _) = parse_comments(f, &["COST".to_string()]).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(kept[0].text.contains("cost"));
        assert_eq!(parse_comments(f, &["المرطب".to_string()]).unwrap().0.len(), 1);
    }

    #[test]
    fn last_page_has_empty_cursor() {
        let (kept, scanned, next) = parse_comments(r#"{"status":"ok","comments":[]}"#, &[]).unwrap();
        assert_eq!((kept.len(), scanned, next.as_str()), (0, 0, ""));
    }

    #[test]
    fn session_errors_are_messages() {
        for body in [
            include_str!("../tests/fixtures/ig_login_required.json"),
            "<!DOCTYPE html><html>login</html>",
            r#"{"message":"checkpoint_required","status":"fail"}"#,
            "",
        ] {
            assert!(parse_comments(body, &[]).unwrap_err().contains("Log in to Instagram"), "{body}");
        }
    }

    fn page(n: usize, next: &str) -> String {
        let comments: Vec<_> = (0..n).map(|i| serde_json::json!({"text": format!("comment number {i} here"), "user": {"username": "u"}})).collect();
        serde_json::json!({"status": "ok", "comments": comments, "next_min_id": next}).to_string()
    }

    async fn run(pages: Vec<ToolResult<String>>, max: usize) -> (ToolResult<CommentPage>, Vec<String>) {
        let asked = std::cell::RefCell::new(Vec::new());
        let pages = std::cell::RefCell::new(pages.into_iter());
        let fetch = |cursor: String| {
            asked.borrow_mut().push(cursor);
            let next = pages.borrow_mut().next().expect("fetched more pages than expected");
            async move { next.and_then(|body| parse_comments(&body, &[])) }
        };
        let out = page_comments(fetch, max, "", Duration::ZERO).await;
        (out, asked.into_inner())
    }

    #[tokio::test]
    async fn pages_until_the_last_cursor() {
        let (out, asked) = run(vec![Ok(page(20, "c 1&x")), Ok(page(5, ""))], 300).await;
        let out = out.unwrap();
        assert_eq!((out.comments.len(), out.scanned, out.next_cursor.as_str()), (25, 25, ""));
        assert_eq!(asked, ["", "c 1&x"]);
        let first = comments_url(64, "").unwrap();
        assert!(first.starts_with("https://www.instagram.com/api/v1/media/64/comments/?") && !first.contains("min_id"));
        let next = comments_url(64, "c 1&x").unwrap();
        assert!(next.contains("min_id=c+1%26x") || next.contains("min_id=c%201%26x"), "{next}");
    }

    #[tokio::test]
    async fn stops_at_max_and_hands_back_the_cursor() {
        let (out, asked) = run(vec![Ok(page(20, "a")), Ok(page(20, "b")), Ok(page(20, "c"))], 30).await;
        let out = out.unwrap();
        assert_eq!((out.scanned, out.next_cursor.as_str(), asked.len()), (40, "b", 2));
    }

    #[tokio::test]
    async fn empty_page_with_a_cursor_does_not_loop_forever() {
        let (out, asked) = run(vec![Ok(page(0, "again"))], 300).await;
        assert_eq!((out.unwrap().scanned, asked.len()), (0, 1));
    }

    #[tokio::test]
    async fn login_page_and_fetch_failures_are_errors() {
        assert!(run(vec![Ok("<html>".into())], 300).await.0.unwrap_err().contains("Log in to Instagram"));
        assert_eq!(run(vec![Err("chrome closed".into())], 300).await.0.unwrap_err(), "chrome closed");
    }

    #[tokio::test]
    async fn failure_after_some_pages_keeps_what_was_read() {
        let limited = r#"{"message":"Please wait a few minutes before you try again.","status":"fail"}"#;
        for bad in [Ok(limited.to_string()), Err("chrome closed".to_string())] {
            let out = run(vec![Ok(page(20, "resume-here")), bad], 300).await.0.unwrap();
            assert_eq!((out.comments.len(), out.scanned, out.next_cursor.as_str()), (20, 20, "resume-here"));
        }
    }

    #[test]
    fn rate_limit_is_not_reported_as_a_login_problem() {
        let err = parse_comments(r#"{"message":"Please wait a few minutes before you try again.","status":"fail"}"#, &[]).unwrap_err();
        assert!(err.contains("Please wait a few minutes") && !err.contains("Log in"), "{err}");
    }

    #[test]
    fn parses_public_search_fixture() {
        let reels = parse_public_reels(include_str!("../tests/fixtures/ig_public_search.json"), 2).unwrap();
        assert_eq!(reels.len(), 2);
        assert_eq!((reels[0].url.as_str(), reels[0].plays, reels[0].user.as_str()), ("https://www.instagram.com/reel/DeHgoKNhQiG/", 12236794, "user2"));
        assert!(reels[1].plays == 6692612 && reels[0].comments.is_none());
        assert!(!serde_json::to_string(&reels[0]).unwrap().contains("comments")); // unknown counts are left out, not shown as 0
    }

    #[test]
    fn parses_public_comments_fixture() {
        let f = include_str!("../tests/fixtures/ig_public_comments.json");
        let (kept, scanned, next) = parse_public_comments(f, &[]).unwrap();
        assert_eq!((kept.len(), scanned), (3, 4)); // the emoji-only comment is noise
        assert!(next.starts_with(r#"{"is_server_cursor_inverse""#)); // passed back verbatim as `after`
        assert_eq!((kept[1].user.as_str(), kept[1].likes, kept[1].replies), ("user2", 7, 2));
        assert_eq!(parse_public_comments(f, &["OILY".into()]).unwrap().0.len(), 1);
        let last = r#"{"data":{"xig_polaris_media":{"comments_connection":{"edges":[],"page_info":{"end_cursor":"x","has_next_page":false}}}}}"#;
        assert_eq!(parse_public_comments(last, &[]).unwrap().2, "");
    }

    #[test]
    fn public_errors_are_messages() {
        let refused = r#"for (;;);{"__ar":1,"error":1357054,"errorSummary":"Your Request Couldn't be Processed","payload":null}"#;
        assert_eq!(parse_public_reels(refused, 5).unwrap_err(), "Instagram refused: Your Request Couldn't be Processed");
        let stale = r#"{"data":null,"errors":[{"message":"execution error"}]}"#;
        assert!(parse_public_comments(stale, &[]).unwrap_err().contains("doc_id may have rotated"));
        assert!(parse_public_comments("<html></html>", &[]).unwrap_err().contains("page instead of data"));
        assert!(parse_public_comments(r#"{"data":{"xig_polaris_media":null}}"#, &[]).unwrap_err().contains("not public"));
        assert!(parse_public_reels(r#"{"data":{"xig_logged_out_popular_search_media_info":null}}"#, 5).unwrap().is_empty());
        assert!(token(9) != token(9) && token(32).len() == 32);
    }

    #[tokio::test]
    #[ignore] // hits instagram.com: cargo test live -- --ignored
    async fn live_public_reels_and_comments() {
        let http = client().unwrap();
        let reels = public_reels(&http, "skin care", 3).await.unwrap();
        let pk = shortcode_from_url(&reels[0].url).and_then(shortcode_to_pk).unwrap();
        let page = public_comments(&http, pk, 40, "", &[]).await.unwrap();
        assert!(page.scanned > 0, "{reels:?}");
    }
}
