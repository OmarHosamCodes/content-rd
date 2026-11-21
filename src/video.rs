//! YouTube and TikTok, logged out: what their web pages and web APIs serve visitors without an account.
use std::time::Duration;

use reqwest::Client;
use serde_json::{Value, json};

use crate::instagram::{Comment, CommentPage, Page, Reel, code_char, keep, page_comments};
use crate::search::{searxng, send};
use crate::ToolResult;

const PAUSE: Duration = Duration::from_millis(500);
// shortcut: YouTube web client version seen on 2026-11-21; YouTube accepts old versions for months. Bump it if comments start failing with HTTP 400.
const YT_CLIENT: &str = "2.20261119.07.00";
const YT_CONSENT: &str = "CONSENT=YES+cb"; // skips the EU cookie-consent page
const TT_COMMENTS: &str = "https://www.tiktok.com/api/comment/list/";

/// youtube.com/watch?v=ID, youtube.com/shorts/ID or youtu.be/ID
pub fn youtube_id(url: &str) -> Option<&str> {
    let id = match url.split_once("youtu.be/") {
        Some((_, rest)) => rest,
        None => {
            let rest = url.split_once("youtube.com/")?.1;
            match rest.strip_prefix("shorts/") {
                Some(id) => id,
                None => rest.strip_prefix("watch")?.split(['?', '&']).find_map(|p| p.strip_prefix("v="))?,
            }
        }
    };
    let id = id.split(['?', '&', '#', '/']).next()?;
    (id.len() == 11 && id.chars().all(code_char)).then_some(id)
}

/// tiktok.com/@user/video/ID
pub fn tiktok_id(url: &str) -> Option<&str> {
    let id = url.split_once("tiktok.com/")?.1.split_once("/video/")?.1.split(['?', '#', '/']).next()?;
    ((15..=21).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_digit())).then_some(id)
}

/// The JSON object a page embeds after `marker`, like `ytInitialData = {...};`.
fn json_after(html: &str, marker: &str) -> Option<Value> {
    let rest = &html[html.find(marker)? + marker.len()..];
    let rest = &rest[rest.find('{')?..];
    serde_json::Deserializer::from_str(rest).into_iter::<Value>().next()?.ok()
}

/// Every value stored under `key`, at any depth, in document order.
fn find<'a>(v: &'a Value, key: &str) -> Vec<&'a Value> {
    fn walk<'a>(v: &'a Value, key: &str, out: &mut Vec<&'a Value>) {
        match v {
            Value::Object(o) => o.iter().for_each(|(k, x)| if k == key { out.push(x) } else { walk(x, key, out) }),
            Value::Array(a) => a.iter().for_each(|x| walk(x, key, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    walk(v, key, &mut out);
    out
}

fn token(v: &Value) -> Option<String> {
    find(v, "continuationCommand").into_iter().next()?.get("token")?.as_str().map(String::from)
}

/// YouTube text in any of its shapes: {simpleText}, {content} or {runs:[{text}]}.
fn yt_text(v: Option<&Value>) -> String {
    let Some(v) = v else { return String::new() };
    match v.get("simpleText").or_else(|| v.get("content")).and_then(Value::as_str) {
        Some(s) => s.to_string(),
        None => v.get("runs").and_then(Value::as_array).into_iter().flatten().filter_map(|r| r.get("text")?.as_str()).collect(),
    }
}

/// "567,290 views", "22K views", "1.2M" -> a number. Pages are asked for English (hl=en), so the format is fixed.
fn yt_count(s: &str) -> u64 {
    let n = s.split_whitespace().next().unwrap_or("").replace(',', "");
    let (num, mul) = match n.chars().last() {
        Some('K') => (&n[..n.len() - 1], 1e3),
        Some('M') => (&n[..n.len() - 1], 1e6),
        Some('B') => (&n[..n.len() - 1], 1e9),
        _ => (&n[..], 1.0),
    };
    num.parse::<f64>().map_or(0, |x| (x * mul).round() as u64)
}

fn reel(url: String, plays: u64, user: String, caption: &str) -> Reel {
    Reel { url, plays, likes: None, comments: None, user, caption: caption.chars().take(160).collect() }
}

fn sort_top(mut reels: Vec<Reel>, limit: usize) -> Vec<Reel> {
    reels.sort_by_key(|r| std::cmp::Reverse(r.plays));
    reels.truncate(limit);
    reels
}

/// Videos and Shorts on a YouTube results page, most views first.
pub fn parse_youtube_search(html: &str, limit: usize) -> ToolResult<Vec<Reel>> {
    let data = json_after(html, "ytInitialData").ok_or("YouTube sent a page without results; it may be blocking this server. Retry later.")?;
    let mut reels: Vec<Reel> = Vec::new();
    let mut add = |r: Reel| if !reels.iter().any(|x| x.url == r.url) { reels.push(r) };
    for v in find(&data, "videoRenderer") {
        if let Some(id) = v.get("videoId").and_then(Value::as_str) {
            add(reel(format!("https://www.youtube.com/watch?v={id}"), yt_count(&yt_text(v.get("viewCountText"))), yt_text(v.get("ownerText")), &yt_text(v.get("title"))));
        }
    }
    for v in find(&data, "shortsLockupViewModel") {
        if let Some(id) = find(v, "reelWatchEndpoint").into_iter().next().and_then(|e| e.get("videoId")?.as_str()) {
            let meta = |k: &str| yt_text(v.pointer(&format!("/overlayMetadata/{k}")));
            add(reel(format!("https://www.youtube.com/shorts/{id}"), yt_count(&meta("secondaryText")), String::new(), &meta("primaryText")));
        }
    }
    Ok(sort_top(reels, limit))
}

/// The token that loads the first page of comments, from a watch page.
pub fn parse_youtube_watch(html: &str) -> ToolResult<String> {
    let data = json_after(html, "ytInitialData").ok_or("YouTube sent a page without video data; it may be blocking this server. Retry later.")?;
    find(&data, "itemSectionRenderer")
        .into_iter()
        .filter(|s| s.get("sectionIdentifier").and_then(Value::as_str) == Some("comment-item-section"))
        .find_map(token)
        .ok_or_else(|| "This video has comments turned off, or it is not public.".into())
}

/// One page of the web client's `next` API. Replies stay folded: only top-level comments are read.
pub fn parse_youtube_comments(body: &str, contains: &[String]) -> ToolResult<Page> {
    let v: Value = serde_json::from_str(body).map_err(|_| "YouTube sent an unreadable comments page; retry.".to_string())?;
    if let Some(e) = v.pointer("/error/message").and_then(Value::as_str) {
        return Err(format!("YouTube refused: {e}"));
    }
    let s = |c: &Value, p: &str| c.pointer(p).and_then(Value::as_str).unwrap_or("").to_string();
    let list: Vec<Comment> = find(&v, "commentEntityPayload")
        .into_iter()
        .map(|c| Comment {
            user: s(c, "/author/displayName").trim_start_matches('@').to_string(),
            text: s(c, "/properties/content/content"),
            likes: yt_count(&s(c, "/toolbar/likeCountNotliked")),
            replies: yt_count(&s(c, "/toolbar/replyCount")),
        })
        .collect();
    let mut actions = find(&v, "reloadContinuationItemsCommand");
    actions.extend(find(&v, "appendContinuationItemsAction"));
    // the next page is a continuation item at the end of the comment list; earlier tokens open reply threads
    let next = actions
        .into_iter()
        .filter(|a| a.get("targetId").and_then(Value::as_str).is_some_and(|t| t.ends_with("comments-section")))
        .filter_map(|a| a.get("continuationItems")?.as_array()?.last()?.get("continuationItemRenderer"))
        .find_map(token)
        .unwrap_or_default();
    let scanned = list.len();
    Ok((keep(list, contains), scanned, next))
}

pub async fn youtube_reels(http: &Client, topic: &str, limit: usize) -> ToolResult<Vec<Reel>> {
    // shortcut: one results page (~20 videos and Shorts) per topic
    let req = http.get("https://www.youtube.com/results").query(&[("search_query", topic), ("hl", "en")]).header("cookie", YT_CONSENT);
    let reels = parse_youtube_search(&send(req, "YouTube").await?, limit)?;
    if reels.is_empty() {
        return Err(format!("No YouTube videos for '{topic}'. Try a broader phrasing."));
    }
    Ok(reels)
}

pub async fn youtube_comments(http: &Client, id: &str, max: usize, cursor: &str, contains: &[String]) -> ToolResult<CommentPage> {
    let start = match cursor {
        "" => parse_youtube_watch(&send(http.get("https://www.youtube.com/watch").query(&[("v", id), ("hl", "en")]).header("cookie", YT_CONSENT), "YouTube").await?)?,
        c => c.to_string(),
    };
    let fetch = |token: String| async move {
        let body = json!({"context": {"client": {"clientName": "WEB", "clientVersion": YT_CLIENT, "hl": "en"}}, "continuation": token});
        parse_youtube_comments(&send(http.post("https://www.youtube.com/youtubei/v1/next?prettyPrint=false").json(&body), "YouTube").await?, contains)
    };
    page_comments(fetch, max, &start, PAUSE).await
}

/// A TikTok video page: its stats and caption.
pub fn parse_tiktok_page(html: &str) -> ToolResult<Reel> {
    let data = json_after(html, r#"id="__UNIVERSAL_DATA_FOR_REHYDRATION__""#).ok_or("TikTok sent a page without video data; it may be blocking this server. Retry later.")?;
    let item = data.pointer("/__DEFAULT_SCOPE__/webapp.video-detail/itemInfo/itemStruct").filter(|i| i.get("id").is_some()).ok_or("TikTok video not found, or it is not public.")?;
    let s = |p: &str| item.pointer(p).and_then(Value::as_str).unwrap_or("");
    let n = |k: &str| item.pointer(&format!("/stats/{k}")).and_then(Value::as_u64);
    let mut r = reel(format!("https://www.tiktok.com/@{}/video/{}", s("/author/uniqueId"), s("/id")), n("playCount").unwrap_or(0), s("/author/uniqueId").into(), s("/desc"));
    (r.likes, r.comments) = (n("diggCount"), n("commentCount"));
    Ok(r)
}

pub fn parse_tiktok_comments(body: &str, contains: &[String]) -> ToolResult<Page> {
    // a blocked request gets HTTP 200 with an empty body
    let v: Value = serde_json::from_str(body).map_err(|_| "TikTok sent an unreadable comments page; it may be blocking this server. Retry later.".to_string())?;
    if v.get("status_code").and_then(Value::as_i64).unwrap_or(0) != 0 {
        return Err(format!("TikTok refused: {}", v["status_msg"].as_str().filter(|m| !m.is_empty()).unwrap_or("no reason given")));
    }
    let list = v.get("comments").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let comments = list
        .iter()
        .filter_map(|c| {
            Some(Comment {
                user: c.pointer("/user/unique_id").and_then(Value::as_str).unwrap_or("").to_string(),
                text: c.get("text")?.as_str()?.to_string(),
                likes: c.get("digg_count").and_then(Value::as_u64).unwrap_or(0),
                replies: c.get("reply_comment_total").and_then(Value::as_u64).unwrap_or(0),
            })
        })
        .collect();
    let next = match v.get("has_more").and_then(Value::as_i64) {
        Some(1) => v.get("cursor").and_then(Value::as_u64).map(|c| c.to_string()).unwrap_or_default(),
        _ => String::new(),
    };
    Ok((keep(comments, contains), list.len(), next))
}

/// TikTok's own search needs a login, so videos are found through the web search engines, then each video page gives its stats.
pub async fn tiktok_reels(http: &Client, searxng_url: Option<&str>, topic: &str, limit: usize) -> ToolResult<Vec<Reel>> {
    let base = searxng_url.ok_or("SEARXNG_URL is not set on the server, so TikTok search is unavailable.")?;
    // shortcut: one search page (10 results) per topic
    let mut urls: Vec<String> = Vec::new();
    for r in searxng(http, base, &format!("site:tiktok.com {topic}"), "all").await? {
        if tiktok_id(&r.url).is_some() && !urls.contains(&r.url) {
            urls.push(r.url);
        }
    }
    if urls.is_empty() {
        return Err(format!("No TikTok videos found for '{topic}'. Try a broader or more common phrasing."));
    }
    let (mut reels, mut why) = (Vec::new(), String::new());
    for (i, url) in urls.iter().take(limit).enumerate() {
        if i > 0 {
            tokio::time::sleep(PAUSE).await;
        }
        match send(http.get(url), "TikTok").await.and_then(|html| parse_tiktok_page(&html)) {
            Ok(r) => reels.push(r),
            Err(e) => why = e, // a deleted or private video just drops out
        }
    }
    if reels.is_empty() {
        return Err(why);
    }
    Ok(sort_top(reels, limit))
}

pub async fn tiktok_comments(http: &Client, id: &str, max: usize, cursor: &str, contains: &[String]) -> ToolResult<CommentPage> {
    // shortcut: logged out, TikTok stops after ~300 top-level comments per video (has_more turns 0); only a login reads further
    let fetch = |cursor: String| async move {
        let cursor = if cursor.is_empty() { "0".to_string() } else { cursor };
        let q = [("aid", "1988"), ("aweme_id", id), ("count", "20"), ("cursor", cursor.as_str())];
        parse_tiktok_comments(&send(http.get(TT_COMMENTS).query(&q).header("referer", "https://www.tiktok.com/"), "TikTok").await?, contains)
    };
    page_comments(fetch, max, cursor, PAUSE).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_from_urls() {
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=xbAs_E5Cp5g&t=3s"), Some("xbAs_E5Cp5g"));
        assert_eq!(youtube_id("https://www.youtube.com/watch?feature=share&v=xbAs_E5Cp5g"), Some("xbAs_E5Cp5g"));
        assert_eq!(youtube_id("https://youtube.com/shorts/bT13cW59r7Q?si=x"), Some("bT13cW59r7Q"));
        assert_eq!(youtube_id("https://youtu.be/xbAs_E5Cp5g"), Some("xbAs_E5Cp5g"));
        assert_eq!(youtube_id("https://www.youtube.com/@channel"), None);
        assert_eq!(youtube_id("https://www.youtube.com/watch?v=short"), None);
        assert_eq!(tiktok_id("https://www.tiktok.com/@a.b/video/7284248855707716896?lang=en"), Some("7284248855707716896"));
        assert_eq!(tiktok_id("https://www.tiktok.com/@a/photo/7284248855707716896"), None);
        assert_eq!(tiktok_id("https://www.tiktok.com/@a/video/12x"), None);
    }

    #[test]
    fn counts() {
        assert_eq!([yt_count("567,290 views"), yt_count("22K views"), yt_count("1.2M"), yt_count("No views"), yt_count("")], [567290, 22000, 1200000, 0, 0]);
    }

    #[test]
    fn parses_youtube_fixtures() {
        let reels = parse_youtube_search(include_str!("../tests/fixtures/yt_search.html"), 2).unwrap();
        assert_eq!(reels.iter().map(|r| (r.url.as_str(), r.plays)).collect::<Vec<_>>(), [("https://www.youtube.com/watch?v=xbAs_E5Cp5g", 567290), ("https://www.youtube.com/shorts/bT13cW59r7Q", 22000)]);
        assert_eq!(reels[0].user, "channel0");
        assert_eq!(parse_youtube_watch(include_str!("../tests/fixtures/yt_watch.html")).unwrap(), "FIRST_COMMENTS_TOKEN");
        let (kept, scanned, next) = parse_youtube_comments(include_str!("../tests/fixtures/yt_comments.json"), &[]).unwrap();
        assert_eq!((kept.len(), scanned, next.as_str()), (2, 3, "NEXT_PAGE_TOKEN")); // the emoji-only comment is dropped
        assert_eq!((kept[0].user.as_str(), kept[1].likes, kept[1].replies), ("user0", 0, 0));
        let (kept, ..) = parse_youtube_comments(include_str!("../tests/fixtures/yt_comments.json"), &["SERUM".into()]).unwrap();
        assert_eq!(kept.len(), 1);
        assert!(parse_youtube_watch("<html>no data</html>").unwrap_err().contains("blocking"));
        assert!(parse_youtube_watch(r#"ytInitialData = {"a": 1};"#).unwrap_err().contains("turned off"));
    }

    #[test]
    fn parses_tiktok_fixtures() {
        let r = parse_tiktok_page(include_str!("../tests/fixtures/tt_page.html")).unwrap();
        assert_eq!((r.url.as_str(), r.plays, r.likes, r.comments), ("https://www.tiktok.com/@creator0/video/7284248855707716896", 659700, Some(29600), Some(165)));
        let (kept, scanned, next) = parse_tiktok_comments(include_str!("../tests/fixtures/tt_comments.json"), &[]).unwrap();
        assert_eq!((kept.len(), scanned, next.as_str()), (2, 3, "20"));
        assert_eq!((kept[0].user.as_str(), kept[0].likes, kept[0].replies), ("user0", 14, 4));
        assert!(parse_tiktok_comments("", &[]).unwrap_err().contains("blocking"));
        assert_eq!(parse_tiktok_comments(r#"{"status_code": 5, "status_msg": "bad"}"#, &[]).unwrap_err(), "TikTok refused: bad");
        assert_eq!(parse_tiktok_comments(r#"{"status_code": 0, "comments": null, "has_more": 0}"#, &[]).unwrap().1, 0);
    }

    #[tokio::test]
    #[ignore]
    async fn live_youtube_and_tiktok() {
        let http = crate::instagram::client().unwrap();
        let videos = youtube_reels(&http, "skin care routine", 3).await.unwrap();
        let id = youtube_id(&videos[0].url).unwrap();
        let page = youtube_comments(&http, id, 40, "", &[]).await.unwrap();
        assert!(page.scanned >= 20 && !page.next_cursor.is_empty(), "{page:?}");
        let page = tiktok_comments(&http, "7284248855707716896", 40, "", &[]).await.unwrap();
        assert!(page.scanned >= 20, "{page:?}");
    }
}
