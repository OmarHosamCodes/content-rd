//! Pure Instagram parsing. The browser work lives in bridge.rs; nothing here touches the network.
use std::collections::HashMap;

use serde::Serialize;
use serde_json::Value;

use crate::ToolResult;

#[derive(Serialize, Debug, Clone)]
pub struct Reel {
    pub url: String,
    pub plays: u64,
    pub likes: u64,
    pub comments: u64,
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
                    likes: n(&["like_count"]),
                    comments: n(&["comment_count"]),
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

/// One page of the comments API -> (kept comments, scanned count, next cursor; empty on the last page).
/// Comments under 3 words are noise. `contains` keeps only comments holding any of the words.
pub fn parse_comments(body: &str, contains: &[String]) -> ToolResult<(Vec<Comment>, usize, String)> {
    let v: Value = serde_json::from_str(body).unwrap_or(Value::Null);
    let Some(list) = v.get("comments").and_then(Value::as_array) else {
        let said = v.get("message").and_then(Value::as_str).unwrap_or("a login page");
        return Err(format!("Log in to Instagram in the bridge's Chrome window, then retry. (Instagram answered: {said})"));
    };
    let needles: Vec<String> = contains.iter().map(|k| k.to_lowercase()).collect();
    let kept = list
        .iter()
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
        .collect();
    let next = v.get("next_min_id").and_then(Value::as_str).unwrap_or("").to_string();
    Ok((kept, list.len(), next))
}

#[derive(Serialize, Debug)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
    pub scanned: usize,
    pub next_cursor: String,
}

/// Scroll a reel's comments from `cursor` until `max` are scanned or Instagram runs out.
/// `fetch` gets a full URL and returns the response body; in the bridge that is a fetch() inside Chrome.
pub async fn page_comments<F, Fut>(mut fetch: F, pk: u64, max: usize, cursor: &str, contains: &[String], pause: std::time::Duration) -> ToolResult<CommentPage>
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = ToolResult<String>>,
{
    let mut out = CommentPage { comments: Vec::new(), scanned: 0, next_cursor: cursor.to_string() };
    loop {
        let mut params = vec![("can_support_threading", "true"), ("permalink_enabled", "false")];
        if !out.next_cursor.is_empty() {
            params.push(("min_id", &out.next_cursor));
        }
        let url = reqwest::Url::parse_with_params(&format!("https://www.instagram.com/api/v1/media/{pk}/comments/"), &params).map_err(|e| e.to_string())?;
        let (kept, scanned, next) = parse_comments(&fetch(url.into()).await?, contains)?;
        out.comments.extend(kept);
        out.scanned += scanned;
        out.next_cursor = next;
        if scanned == 0 || out.next_cursor.is_empty() || out.scanned >= max {
            return Ok(out);
        }
        tokio::time::sleep(pause).await; // stay gentle: fast paging gets accounts flagged
    }
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
        assert_eq!((reels[1].comments, reels[1].caption.as_str()), (900, "my routine"));
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
        let fetch = |url: String| {
            asked.borrow_mut().push(url);
            let next = pages.borrow_mut().next().expect("fetched more pages than expected");
            async move { next }
        };
        let out = page_comments(fetch, 64, max, "", &[], Duration::ZERO).await;
        (out, asked.into_inner())
    }

    #[tokio::test]
    async fn pages_until_the_last_cursor() {
        let (out, asked) = run(vec![Ok(page(20, "c 1&x")), Ok(page(5, ""))], 300).await;
        let out = out.unwrap();
        assert_eq!((out.comments.len(), out.scanned, out.next_cursor.as_str()), (25, 25, ""));
        assert!(asked[0].starts_with("https://www.instagram.com/api/v1/media/64/comments/?") && !asked[0].contains("min_id"));
        assert!(asked[1].contains("min_id=c+1%26x") || asked[1].contains("min_id=c%201%26x"), "{}", asked[1]);
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
        assert_eq!(run(vec![Ok(page(20, "a")), Err("chrome closed".into())], 300).await.0.unwrap_err(), "chrome closed");
    }
}
