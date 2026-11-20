//! Boards: JSON files on disk, rendered as a public read-only page.
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;

use maud::{DOCTYPE, Markup, PreEscaped, html};
use serde::{Deserialize, Serialize};

use crate::ToolResult;

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Card {
    pub pattern: String,
    pub user: String,
    pub text: String,
    pub likes: u64,
    pub reel_url: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Link {
    pub title: String,
    pub url: String,
}

#[derive(Serialize, Deserialize, Default, Clone, Debug)]
#[serde(default)]
pub struct Gap {
    pub keyword: String,
    pub interest: i64,
    pub scores: BTreeMap<String, i64>,
    pub verdict: String,
    pub results: Vec<Link>,
}

#[derive(Serialize, Deserialize, Default, Debug)]
#[serde(default)]
pub struct Board {
    pub comments: Vec<Card>,
    pub gaps: Vec<Gap>,
}

/// `[a-z0-9][a-z0-9-]{0,63}`: the only thing that ever becomes a file name.
pub fn valid_name(name: &str) -> bool {
    let ok = |c: u8| c.is_ascii_lowercase() || c.is_ascii_digit();
    let b = name.as_bytes();
    (1..=64).contains(&b.len()) && ok(b[0]) && b.iter().all(|&c| ok(c) || c == b'-')
}

impl Board {
    pub fn add_comments(&mut self, cards: Vec<Card>) {
        let mut seen: HashSet<(String, String)> = self.comments.iter().map(|c| (c.user.clone(), c.text.clone())).collect();
        for c in cards {
            if seen.insert((c.user.clone(), c.text.clone())) {
                self.comments.push(c);
            }
        }
    }

    pub fn upsert_gap(&mut self, gap: Gap) {
        self.gaps.retain(|g| g.keyword != gap.keyword);
        self.gaps.push(gap);
    }
}

pub struct Store {
    dir: PathBuf,
    lock: tokio::sync::Mutex<()>,
}

impl Store {
    pub fn new(dir: PathBuf) -> Store {
        Store { dir, lock: tokio::sync::Mutex::new(()) }
    }

    fn path(&self, name: &str) -> ToolResult<PathBuf> {
        if !valid_name(name) {
            return Err("board name must be lowercase letters, digits and dashes (max 64)".into());
        }
        Ok(self.dir.join(format!("{name}.json")))
    }

    pub fn load(&self, name: &str) -> ToolResult<Option<Board>> {
        let path = self.path(name)?;
        match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map(Some).map_err(|e| format!("board {name} is corrupt: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(format!("cannot read board {name}: {e}")),
        }
    }

    /// Load-modify-save under a lock so parallel tool calls do not lose each other's writes.
    pub async fn update(&self, name: &str, f: impl FnOnce(&mut Board)) -> ToolResult<()> {
        let path = self.path(name)?;
        let _guard = self.lock.lock().await;
        let mut board = self.load(name)?.unwrap_or_default();
        f(&mut board);
        let io = |e: std::io::Error| format!("cannot save board {name}: {e}");
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let tmp = path.with_extension("json.tmp");
        let json = serde_json::to_string_pretty(&board).map_err(|e| e.to_string())?;
        std::fs::write(&tmp, json).map_err(io)?;
        std::fs::rename(&tmp, &path).map_err(io) // rename is atomic: a crash never leaves half a board
    }
}

const CSS: &str = "
:root{color-scheme:light dark;--bg:#f6f5f1;--card:#fff;--ink:#1c1b19;--mute:#6f6b63;--line:#e3e0d8;--acc:#b4441f}
@media(prefers-color-scheme:dark){:root{--bg:#161513;--card:#211f1c;--ink:#f1eee8;--mute:#a09a8f;--line:#35322d;--acc:#f08a5d}}
*{box-sizing:border-box}body{margin:0;padding:32px 20px 64px;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,sans-serif}
main{max-width:1240px;margin:auto}h1{font-size:28px;margin:0 0 4px}h2{font-size:19px;margin:44px 0 4px}h3{font-size:14px;margin:26px 0 10px;color:var(--acc)}
.sub{color:var(--mute);margin:0}a{color:inherit}
.grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(250px,1fr));gap:12px}
.card{height:190px;display:flex;flex-direction:column;background:var(--card);border:1px solid var(--line);border-radius:10px;padding:14px}
.card p{margin:0;flex:1;overflow:hidden;display:-webkit-box;-webkit-box-orient:vertical;-webkit-line-clamp:5;line-clamp:5}
.card footer{display:flex;justify-content:space-between;gap:8px;color:var(--mute);font-size:12.5px;padding-top:8px;white-space:nowrap}
.card footer span:first-child{overflow:hidden;text-overflow:ellipsis}
.wrap{overflow-x:auto}table{width:100%;border-collapse:collapse;background:var(--card);border:1px solid var(--line)}
th,td{padding:10px 12px;text-align:start;border-bottom:1px solid var(--line);vertical-align:top}
th{font-size:12px;text-transform:uppercase;letter-spacing:.04em;color:var(--mute)}td.n,th.n{text-align:end;font-variant-numeric:tabular-nums}
.total{font-weight:700;color:var(--acc)}details{margin-top:6px;color:var(--mute);font-size:13px}summary{cursor:pointer}ol{margin:6px 0 0;padding-inline-start:20px}
";

/// A link only for https URLs; anything else (javascript:, data:) is shown as plain text.
fn link(url: &str, label: &str) -> Markup {
    html! {
        @if url.starts_with("https://") { a href=(url) target="_blank" rel="noopener noreferrer" { (label) } }
        @else { (label) }
    }
}

pub fn render(name: &str, board: &Board) -> Markup {
    let total = |g: &Gap| g.scores.values().sum::<i64>();
    let mut gaps: Vec<&Gap> = board.gaps.iter().collect();
    gaps.sort_by_key(|g| std::cmp::Reverse(total(g)));
    let mut dims: Vec<&String> = Vec::new();
    for d in gaps.iter().flat_map(|g| g.scores.keys()) {
        if !dims.contains(&d) {
            dims.push(d);
        }
    }
    let mut groups: Vec<(&str, Vec<&Card>)> = Vec::new();
    for c in &board.comments {
        match groups.iter_mut().find(|(p, _)| *p == c.pattern) {
            Some((_, cards)) => cards.push(c),
            None => groups.push((&c.pattern, vec![c])),
        }
    }
    groups.sort_by_key(|(_, cards)| std::cmp::Reverse(cards.len()));
    for (_, cards) in &mut groups {
        cards.sort_by_key(|c| std::cmp::Reverse(c.likes));
    }
    html! {
        (DOCTYPE)
        html lang="en" {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width,initial-scale=1";
            title { (name) " - Content R&D" }
            style { (PreEscaped(CSS)) }
            main {
                h1 { (name) }
                p.sub { (board.comments.len()) " comments, " (board.gaps.len()) " search gaps" }
                @if !gaps.is_empty() {
                    h2 { "Search gaps" }
                    p.sub { "Ranked by total score. Interest is relative Google Trends demand, not monthly volume." }
                    div.wrap { table {
                        tr {
                            th { "#" } th { "Keyword" } th.n { "Interest" }
                            @for d in &dims { th.n { (d.replace('_', " ")) } }
                            th.n { "Total" }
                        }
                        @for (i, g) in gaps.iter().enumerate() {
                            tr {
                                td { (i + 1) }
                                td {
                                    strong dir="auto" { (g.keyword) }
                                    div dir="auto" { (g.verdict) }
                                    @if !g.results.is_empty() {
                                        details {
                                            summary { "Current top results" }
                                            ol { @for r in &g.results { li { (link(&r.url, if r.title.is_empty() { &r.url } else { &r.title })) } } }
                                        }
                                    }
                                }
                                td.n { (g.interest) }
                                @for d in &dims { td.n { @if let Some(v) = g.scores.get(*d) { (v) } } }
                                td.n.total { (total(g)) }
                            }
                        }
                    } }
                }
                @if !groups.is_empty() {
                    h2 { "Comment patterns" }
                    @for (pattern, cards) in &groups {
                        h3 dir="auto" { (pattern) " (" (cards.len()) ")" }
                        div.grid {
                            @for c in cards {
                                article.card {
                                    p dir="auto" title=(c.text) { (c.text) }
                                    footer { span { "@" (c.user) } span { (c.likes) " likes " (link(&c.reel_url, "reel")) } }
                                }
                            }
                        }
                    }
                }
                @if gaps.is_empty() && groups.is_empty() { p { "Nothing here yet." } }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(text: &str) -> Card {
        Card { pattern: "p".into(), user: "u".into(), text: text.into(), likes: 1, reel_url: String::new() }
    }

    fn gap(keyword: &str, score: i64, interest: i64) -> Gap {
        Gap { keyword: keyword.into(), interest, scores: [("demand".to_string(), score)].into(), verdict: "v".into(), results: vec![] }
    }

    #[test]
    fn names() {
        for ok in ["a", "skincare-eg", "x9"] {
            assert!(valid_name(ok), "{ok}");
        }
        for bad in ["", "A", "../x", "a/b", "-a", "a b", "a.json", &"a".repeat(65)] {
            assert!(!valid_name(bad), "{bad}");
        }
        assert!(valid_name(&"a".repeat(64)));
    }

    #[test]
    fn dedupes_comments() {
        let mut b = Board::default();
        b.add_comments(vec![card("t"), card("t")]);
        b.add_comments(vec![card("t"), card("other")]);
        assert_eq!(b.comments.len(), 2);
    }

    #[test]
    fn upsert_replaces_same_keyword() {
        let mut b = Board::default();
        b.upsert_gap(gap("k", 1, 10));
        b.upsert_gap(gap("k", 2, 20));
        assert_eq!(b.gaps.len(), 1);
        assert_eq!(b.gaps[0].interest, 20);
    }

    #[test]
    fn render_is_inert_and_ranked() {
        let mut b = Board::default();
        b.add_comments(vec![Card {
            pattern: "<b>p".into(),
            user: "\" onmouseover=\"x".into(),
            text: "<script>alert(1)</script>".into(),
            likes: 3,
            reel_url: "javascript:alert(1)".into(),
        }]);
        b.upsert_gap(gap("kw-b", 1, 1));
        let mut top = gap("kw-a", 5, 9);
        top.results = vec![Link { title: "T".into(), url: "https://e.com/x".into() }, Link { title: "bad".into(), url: "javascript:alert(2)".into() }];
        b.upsert_gap(top);
        let html = render("t", &b).into_string();
        assert!(!html.contains("<script>alert") && !html.contains("javascript:") && !html.contains("<b>p"));
        assert!(!html.contains("\" onmouseover"));
        assert!(html.contains("href=\"https://e.com/x\""));
        assert!(html.find("kw-a").unwrap() < html.find("kw-b").unwrap());
    }

    #[tokio::test]
    async fn store_roundtrip_and_rejects_bad_names() {
        let root = tempfile::tempdir().unwrap();
        let store = Store::new(root.path().join("boards"));
        store.update("demo", |b| b.add_comments(vec![card("hello there friend")])).await.unwrap();
        store.update("demo", |b| b.add_comments(vec![card("second one here")])).await.unwrap();
        assert_eq!(store.load("demo").unwrap().unwrap().comments.len(), 2);
        assert!(store.update("../x", |_| {}).await.is_err());
        assert!(store.load("../x").is_err());
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 1); // only boards/
        assert!(store.load("nope").unwrap().is_none());
    }

    #[tokio::test]
    async fn corrupt_board_is_an_error_not_a_reset() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("demo.json"), "{not json").unwrap();
        let store = Store::new(root.path().to_path_buf());
        assert!(store.update("demo", |b| b.add_comments(vec![card("a b c")])).await.is_err());
        assert_eq!(std::fs::read_to_string(root.path().join("demo.json")).unwrap(), "{not json");
    }

    #[test]
    fn loads_python_board() {
        let b: Board = serde_json::from_str(include_str!("../tests/fixtures/board_python.json")).unwrap();
        assert_eq!((b.comments.len(), b.gaps.len()), (2, 1));
    }
}
