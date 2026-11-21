//! The MCP surface: 7 tools, one prompt, and the workflow instructions every agent receives.
use std::collections::BTreeMap;
use std::sync::Arc;

use rmcp::handler::server::router::{prompt::PromptRouter, tool::ToolRouter};
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock, PromptMessage, Role, ServerCapabilities, ServerConfig};
use rmcp::schemars::{self, JsonSchema};
use rmcp::{ErrorData as McpError, ServerHandler, prompt, prompt_handler, prompt_router, tool, tool_handler, tool_router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::board::{Card, Gap, Link};
use crate::{State, ToolResult, instagram, search, video};

pub const WORKFLOW: &str = "Content R&D workflow. Pick one board name (lowercase-with-dashes) and reuse it in every board_* call.

A) Comment mining on Instagram, TikTok and YouTube. All three are read without a login; failed Instagram calls retry through the user's local bridge when it runs. If a platform fails, tell the user and carry on with the others and part B.
Call find_reels and get_comments one at a time, never in parallel: parallel calls get rate-limited.
1. Choose 3-5 topics for the industry. For each, on every platform: find_reels(topic, platform) -> highest-exposure videos. Instagram's public search knows only some phrasings: if a topic finds nothing there, try a more common or two-word form.
2. For the top videos: get_comments(url) reads up to 300 comments per call. Page with next_cursor until ~1000 are scanned per video (TikTok stops at ~300 without a login).
3. Keep only comments that reveal a real pain, question, objection, desire or misconception. Expect ~5 per 1000.
4. For each insight, hunt for a pattern: call get_comments on other videos, on every platform, with contains=[keywords of that insight]. An insight that repeats across platforms is a strong pattern.
5. Save with board_add_comments(board, pattern, comments) - one call per pattern, pattern = short label of the insight; set each comment's reel_url to the video it came from.

B) Search gaps
1. Brainstorm 20+ seed keywords, expand with keyword_ideas.
2. keyword_interest on the candidates. If interest is low, try new keywords until some score high.
3. For each high-interest keyword: serp(keyword). Judge the top results: weak, outdated or unrelated = gap.
4. Save each promising keyword with board_add_gap, scoring 1-5 on: demand, serp_weakness, relevance, ease.
5. Stop at 5+ saved gaps. The board ranks them by total score.

Finish by giving the user the board URL and the top 3 recommendations.";

fn en() -> String { "en".into() }
fn us() -> String { "us".into() }
fn ten() -> usize { 10 }
fn three_hundred() -> usize { MAX_COMMENTS_PER_CALL }

/// About 30s of paging (1s pause per ~17-comment page): keeps one call inside client and relay timeouts.
const MAX_COMMENTS_PER_CALL: usize = 300;

#[derive(Deserialize, JsonSchema, Default, Clone, Copy)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    #[default]
    Instagram,
    Tiktok,
    Youtube,
}

#[derive(Deserialize, JsonSchema)]
pub struct FindReels {
    /// Topic or hashtag to search
    topic: String,
    #[serde(default)]
    platform: Platform,
    #[serde(default = "ten")]
    limit: usize,
}

#[derive(Deserialize, JsonSchema)]
pub struct GetComments {
    /// Instagram reel, YouTube video or Short, or TikTok video (https://www.tiktok.com/@user/video/<id>)
    #[serde(alias = "reel_url")]
    url: String,
    #[serde(default = "three_hundred")]
    max_comments: usize,
    /// next_cursor from the previous call, to keep scrolling
    #[serde(default)]
    cursor: String,
    /// keep only comments containing any of these words
    #[serde(default)]
    contains: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeywordIdeas {
    seeds: Vec<String>,
    #[serde(default = "en")]
    lang: String,
    #[serde(default = "us")]
    country: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct KeywordInterest {
    keywords: Vec<String>,
    /// Country code like EG or US. Empty = worldwide
    #[serde(default)]
    geo: String,
    /// Defaults to the first keyword. Use a well-known term
    #[serde(default)]
    anchor: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct Serp {
    keyword: String,
    #[serde(default = "en")]
    lang: String,
    #[serde(default = "us")]
    country: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct CommentIn {
    #[serde(default)]
    user: String,
    text: String,
    #[serde(default)]
    likes: u64,
    #[serde(default)]
    reel_url: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct BoardAddComments {
    board: String,
    /// Short label of the insight these comments share
    pattern: String,
    comments: Vec<CommentIn>,
    /// Video the comments came from, used when a comment has no reel_url of its own
    #[serde(default)]
    reel_url: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct LinkIn {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
}

#[derive(Deserialize, JsonSchema)]
pub struct BoardAddGap {
    board: String,
    keyword: String,
    /// keyword_interest score
    interest: i64,
    /// 1-5 per dimension, e.g. demand, serp_weakness, relevance, ease
    scores: BTreeMap<String, i64>,
    /// One sentence on why the current results are weak
    verdict: String,
    /// serp output
    #[serde(default)]
    top_results: Vec<LinkIn>,
}

#[derive(Deserialize, JsonSchema)]
pub struct ContentRdArgs {
    industry: String,
    /// Market and language, e.g. "Egypt, Arabic"
    market: Option<String>,
}

fn value<T: Serialize>(result: ToolResult<T>) -> ToolResult<serde_json::Value> {
    result.and_then(|v| serde_json::to_value(v).map_err(|e| e.to_string()))
}

/// Tool failures go back as tool errors the agent can read and act on.
fn out<T: Serialize>(result: ToolResult<T>) -> Result<CallToolResult, McpError> {
    let block = |s: String| vec![ContentBlock::text(s)];
    Ok(match result.and_then(|v| serde_json::to_string(&v).map_err(|e| e.to_string())) {
        Ok(json) => CallToolResult::success(block(json)),
        Err(e) => CallToolResult::error(block(e)),
    })
}

#[derive(Clone)]
pub struct ContentRd {
    state: Arc<State>,
    tool_router: ToolRouter<Self>,
    prompt_router: PromptRouter<Self>,
}

#[tool_router]
impl ContentRd {
    pub fn new(state: Arc<State>) -> Self {
        Self { state, tool_router: Self::tool_router(), prompt_router: Self::prompt_router() }
    }

    #[tool(description = "Find the highest-exposure short videos for a topic on instagram (default), tiktok or youtube, most plays first: [{url, plays, user, caption}], likes/comments when the platform shows them. Up to 12 per topic on Instagram, 10 on TikTok, ~20 on YouTube.")]
    async fn find_reels(&self, Parameters(a): Parameters<FindReels>) -> Result<CallToolResult, McpError> {
        let (s, limit) = (&self.state, a.limit.clamp(1, 50));
        out(match a.platform {
            Platform::Instagram => {
                let direct = value(instagram::public_reels(&s.public, &a.topic, limit).await);
                self.or_bridge(direct, "find_reels", json!({"topic": a.topic, "limit": limit})).await
            }
            Platform::Youtube => value(video::youtube_reels(&s.public, &a.topic, limit).await),
            Platform::Tiktok => value(video::tiktok_reels(&s.public, s.cfg.searxng_url.as_deref(), &a.topic, limit).await),
        })
    }

    /// The logged-out API failed: retry once through the user's logged-in bridge, keeping both reasons if that fails too.
    async fn or_bridge(&self, direct: ToolResult<serde_json::Value>, tool: &str, args: serde_json::Value) -> ToolResult<serde_json::Value> {
        let Err(why) = direct else { return direct };
        self.state.relay.call(tool, args).await.map_err(|bridge| format!("{why}\nFallback through the local bridge failed too: {bridge}"))
    }

    #[tool(description = "Read top-level comments of an Instagram reel, YouTube video or TikTok video. Returns {comments:[{user,text,likes,replies}], scanned, next_cursor}. Reads up to 300 comments per call; pass next_cursor back as cursor (same url) to keep scrolling. Call one at a time. Comments under 3 words are dropped as noise.")]
    async fn get_comments(&self, Parameters(a): Parameters<GetComments>) -> Result<CallToolResult, McpError> {
        let (s, max) = (&self.state, a.max_comments.clamp(1, MAX_COMMENTS_PER_CALL));
        if let Some(id) = video::youtube_id(&a.url) {
            return out(video::youtube_comments(&s.public, id, max, &a.cursor, &a.contains).await);
        }
        if let Some(id) = video::tiktok_id(&a.url) {
            return out(video::tiktok_comments(&s.public, id, max, &a.cursor, &a.contains).await);
        }
        let Some(pk) = instagram::shortcode_from_url(&a.url).and_then(instagram::shortcode_to_pk) else {
            return out::<()>(Err("url must be an Instagram reel (https://www.instagram.com/reel/<code>/), a YouTube video, or a TikTok video (https://www.tiktok.com/@user/video/<id>)".into()));
        };
        let direct = value(instagram::public_comments(&s.public, pk, max, &a.cursor, &a.contains).await);
        // shortcut: a cursor from one API means nothing to the other, so the bridge restarts from the top on fallback
        let cursor = if direct.is_err() { "" } else { &a.cursor };
        out(self.or_bridge(direct, "get_comments", json!({"reel_url": a.url, "max_comments": max, "cursor": cursor, "contains": a.contains})).await)
    }

    #[tool(description = "Expand seed keywords with what people actually type (Google autocomplete). lang/country like ar/eg.")]
    async fn keyword_ideas(&self, Parameters(a): Parameters<KeywordIdeas>) -> Result<CallToolResult, McpError> {
        out(search::keyword_ideas(&self.state.http, &a.seeds, &a.lang, &a.country).await)
    }

    #[tool(description = "Search demand over the last 12 months from Google Trends, relative to the anchor keyword (anchor = 100). These are relative numbers, not monthly search volumes.")]
    async fn keyword_interest(&self, Parameters(a): Parameters<KeywordInterest>) -> Result<CallToolResult, McpError> {
        out(search::keyword_interest(&self.state.http, &a.keywords, &a.geo, &a.anchor).await)
    }

    #[tool(description = "Top web results for a keyword: [{title, url, snippet, engines}]. engines lists which search engines returned each result. Judge them: weak or unrelated results = content gap.")]
    async fn serp(&self, Parameters(a): Parameters<Serp>) -> Result<CallToolResult, McpError> {
        out(search::serp(&self.state.http, self.state.cfg.searxng_url.as_deref(), &a.keyword, &a.lang, &a.country).await)
    }

    #[tool(description = "Pin valuable comments to the board under a pattern label. Returns the board URL.")]
    async fn board_add_comments(&self, Parameters(a): Parameters<BoardAddComments>) -> Result<CallToolResult, McpError> {
        let cards = a.comments.into_iter().map(|c| Card {
            pattern: a.pattern.clone(),
            user: c.user,
            text: c.text,
            likes: c.likes,
            reel_url: if c.reel_url.is_empty() { a.reel_url.clone() } else { c.reel_url },
        });
        let cards: Vec<Card> = cards.collect();
        out(self.state.store.update(&a.board, |b| b.add_comments(cards)).await.map(|_| self.url(&a.board)))
    }

    #[tool(description = "Save a search-gap keyword with 1-5 scores per dimension. The board ranks gaps by total score. Returns the board URL.")]
    async fn board_add_gap(&self, Parameters(a): Parameters<BoardAddGap>) -> Result<CallToolResult, McpError> {
        let gap = Gap {
            keyword: a.keyword,
            interest: a.interest,
            scores: a.scores,
            verdict: a.verdict,
            results: a.top_results.into_iter().take(10).map(|r| Link { title: r.title, url: r.url }).collect(),
        };
        out(self.state.store.update(&a.board, |b| b.upsert_gap(gap)).await.map(|_| self.url(&a.board)))
    }

    fn url(&self, board: &str) -> String {
        format!("{}/b/{board}", self.state.cfg.base_url)
    }
}

#[prompt_router]
impl ContentRd {
    #[prompt(name = "content_rd", description = "Run the full Content R&D workflow for an industry")]
    async fn content_rd(&self, Parameters(a): Parameters<ContentRdArgs>) -> Vec<PromptMessage> {
        let market = a.market.unwrap_or_else(|| "infer from the industry".into());
        vec![PromptMessage::new_text(Role::User, format!("Run Content R&D for the industry: {}. Market/language: {market}.\n\n{WORKFLOW}", a.industry))]
    }
}

#[tool_handler(router = self.tool_router)]
#[prompt_handler(router = self.prompt_router)]
impl ServerHandler for ContentRd {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().enable_prompts().build()).with_instructions(WORKFLOW)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn failed_public_call_falls_back_to_a_small_bridge_job() {
        // nothing listens on port 1, so the public API fails at once and the bridge must take over
        let dead = reqwest::Client::builder().proxy(reqwest::Proxy::all("http://127.0.0.1:1").unwrap()).build().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let relay = Arc::new(crate::relay::Relay::new(Duration::from_millis(500), Duration::from_millis(200)));
        let cfg = crate::Config { port: 0, token: "t".into(), data_dir: dir.path().into(), base_url: "http://localhost".into(), searxng_url: None };
        let state = Arc::new(State { store: crate::board::Store::new(dir.path().into()), cfg, relay: relay.clone(), http: reqwest::Client::new(), public: dead });
        let bridge = tokio::spawn(async move {
            let job = relay.next_job().await.unwrap();
            relay.finish(job.id, Ok(json!({})));
            job.args
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        let args = GetComments { url: "https://www.instagram.com/reel/Cabc/".into(), max_comments: 1000, cursor: "public-cursor".into(), contains: vec![] };
        let res = ContentRd::new(state).get_comments(Parameters(args)).await.unwrap();
        assert_ne!(res.is_error, Some(true));
        let job = bridge.await.unwrap();
        assert_eq!((job["max_comments"].as_u64(), job["cursor"].as_str()), (Some(300), Some("")));
        assert!(WORKFLOW.contains("one at a time"));
    }
}
