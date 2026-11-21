//! Search-gap research: Google autocomplete, Google Trends, and SERPs through a private SearXNG.
use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::Client;
use serde::Serialize;
use serde_json::Value;

use crate::ToolResult;

const UA: &str = "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36";
const TRENDS: &str = "https://trends.google.com/trends";

#[derive(Serialize, Debug)]
pub struct SerpResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
    pub engines: Vec<String>,
}

pub fn client() -> ToolResult<Client> {
    Client::builder().user_agent(UA).cookie_store(true).timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())
}

/// Google prefixes Trends JSON with `)]}'` to stop it being loaded as a script.
pub fn strip_guard(text: &str) -> ToolResult<Value> {
    text.find('{')
        .and_then(|i| serde_json::from_str(&text[i..]).ok())
        .ok_or_else(|| "Google Trends sent an unreadable answer; wait a minute and retry.".to_string())
}

/// Trends compares at most 5 terms. Every batch starts with the anchor so scores share one scale.
pub fn batches(keywords: &[String], anchor: &str) -> Vec<Vec<String>> {
    let mut rest: Vec<&String> = Vec::new();
    for k in keywords {
        if k != anchor && !rest.contains(&k) {
            rest.push(k);
        }
    }
    rest.chunks(4).map(|c| std::iter::once(anchor.to_string()).chain(c.iter().map(|k| k.to_string())).collect()).collect()
}

/// Mean interest per keyword over the timeline. No data counts as zero interest.
pub fn means(widget: &Value, n: usize) -> Vec<f64> {
    let rows = widget.pointer("/default/timelineData").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&[]);
    (0..n)
        .map(|i| if rows.is_empty() { 0.0 } else { rows.iter().map(|r| r["value"][i].as_f64().unwrap_or(0.0)).sum::<f64>() / rows.len() as f64 })
        .collect()
}

/// Scores of means[1..] relative to means[0] = 100. None when the anchor has no interest.
pub fn relative(means: &[f64]) -> Option<Vec<i64>> {
    let anchor = *means.first().filter(|a| **a > 0.0)?;
    Some(means[1..].iter().map(|m| (m / anchor * 100.0).round() as i64).collect())
}

pub fn parse_searxng(body: &Value) -> Vec<SerpResult> {
    let text = |r: &Value, k: &str| r.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    body.get("results")
        .and_then(Value::as_array)
        .map(|rs| {
            rs.iter()
                .filter(|r| r.get("url").is_some_and(Value::is_string))
                .take(10)
                .map(|r| SerpResult {
                    title: text(r, "title"),
                    url: text(r, "url"),
                    snippet: text(r, "content").chars().take(300).collect(),
                    engines: r["engines"].as_array().map(|e| e.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default()
}

async fn get(http: &Client, url: &str, query: &[(&str, &str)], who: &str) -> ToolResult<String> {
    send(http.get(url).query(query), who).await
}

/// Send a request and return the body, or an error the agent can act on. `who` names the site in errors.
pub async fn send(req: reqwest::RequestBuilder, who: &str) -> ToolResult<String> {
    let r = req.send().await.map_err(|e| format!("{who} is unreachable: {e}"))?;
    if !r.status().is_success() {
        return Err(format!("{who} returned {}; wait a minute and retry.", r.status()));
    }
    r.text().await.map_err(|e| format!("{who} sent an unreadable answer: {e}"))
}

pub async fn keyword_ideas(http: &Client, seeds: &[String], lang: &str, country: &str) -> ToolResult<BTreeMap<String, Vec<String>>> {
    let mut out = BTreeMap::new();
    for seed in seeds {
        let q = [("client", "firefox"), ("ie", "utf-8"), ("oe", "utf-8"), ("hl", lang), ("gl", country), ("q", seed.as_str())];
        let body = get(http, "https://suggestqueries.google.com/complete/search", &q, "Google autocomplete").await?;
        let v: Value = serde_json::from_str(&body).map_err(|_| "Google autocomplete sent an unreadable answer.".to_string())?;
        let ideas = v[1].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        out.insert(seed.clone(), ideas);
    }
    Ok(out)
}

pub async fn keyword_interest(http: &Client, keywords: &[String], geo: &str, anchor: &str) -> ToolResult<BTreeMap<String, i64>> {
    let anchor = if anchor.is_empty() { keywords.first().ok_or("keywords is empty")?.as_str() } else { anchor };
    let mut out = BTreeMap::from([(anchor.to_string(), 100)]);
    get(http, &format!("{TRENDS}/"), &[("geo", geo)], "Google Trends").await?; // sets the cookie the API requires
    for (i, batch) in batches(keywords, anchor).iter().enumerate() {
        if i > 0 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        let items: Vec<Value> = batch.iter().map(|k| serde_json::json!({"keyword": k, "geo": geo, "time": "today 12-m"})).collect();
        let req = serde_json::json!({"comparisonItem": items, "category": 0, "property": ""}).to_string();
        let explore = strip_guard(&get(http, &format!("{TRENDS}/api/explore"), &[("hl", "en-US"), ("tz", "0"), ("req", &req)], "Google Trends").await?)?;
        let widget = explore["widgets"].as_array().and_then(|w| w.iter().find(|w| w["id"] == "TIMESERIES")).ok_or("Google Trends sent no timeline; retry.")?;
        let q = [("hl", "en-US"), ("tz", "0"), ("req", &widget["request"].to_string()), ("token", widget["token"].as_str().unwrap_or(""))];
        let data = strip_guard(&get(http, &format!("{TRENDS}/api/widgetdata/multiline"), &q, "Google Trends").await?)?;
        let scores = relative(&means(&data, batch.len()))
            .ok_or_else(|| format!("Anchor '{anchor}' has no search interest in {}; pick a more popular anchor.", if geo.is_empty() { "worldwide" } else { geo }))?;
        out.extend(batch[1..].iter().cloned().zip(scores));
    }
    Ok(out)
}

/// `language` is like en-US, or "all".
pub async fn searxng(http: &Client, base: &str, query: &str, language: &str) -> ToolResult<Vec<SerpResult>> {
    let body = get(http, &format!("{}/search", base.trim_end_matches('/')), &[("q", query), ("format", "json"), ("language", language)], "SearXNG").await?;
    Ok(parse_searxng(&serde_json::from_str(&body).map_err(|_| "SearXNG sent an unreadable answer.".to_string())?))
}

pub async fn serp(http: &Client, searxng_url: Option<&str>, keyword: &str, lang: &str, country: &str) -> ToolResult<Vec<SerpResult>> {
    let base = searxng_url.ok_or("SEARXNG_URL is not set on the server, so serp is unavailable.")?;
    let results = searxng(http, base, keyword, &format!("{lang}-{}", country.to_uppercase())).await?;
    if results.is_empty() {
        return Err(format!("No search results for '{keyword}'. The search engines may be rate-limiting; retry in a minute."));
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn guard() {
        assert_eq!(strip_guard(")]}'\n{\"a\":1}").unwrap()["a"], 1);
        assert_eq!(strip_guard(")]}',\n{\"a\":2}").unwrap()["a"], 2);
        assert!(strip_guard("<html>429</html>").is_err());
        assert!(strip_guard("").is_err());
    }

    #[test]
    fn batching() {
        let b = batches(&s(&["a", "b", "c", "d", "e", "f", "b", "a"]), "a");
        assert_eq!(b, vec![s(&["a", "b", "c", "d", "e"]), s(&["a", "f"])]); // anchor first, max 5, deduped
        assert_eq!(batches(&s(&["x"]), "anchor"), vec![s(&["anchor", "x"])]);
        assert!(batches(&s(&["a"]), "a").is_empty()); // nothing to compare against the anchor
    }

    #[test]
    fn relative_scores() {
        assert_eq!(relative(&[50.0, 25.0, 130.0, 0.0]), Some(vec![50, 260, 0])); // anchor dropped, anchor = 100
        assert_eq!(relative(&[0.0, 10.0]), None);
        assert_eq!(relative(&[]), None);
    }

    #[test]
    fn timeline_means() {
        let v = serde_json::json!({"default": {"timelineData": [{"value": [10, 0]}, {"value": [30, 4]}]}});
        assert_eq!(means(&v, 2), vec![20.0, 2.0]);
        assert_eq!(means(&serde_json::json!({"default": {"timelineData": []}}), 2), vec![0.0, 0.0]); // no data = no interest
        assert_eq!(means(&serde_json::json!({}), 1), vec![0.0]);
    }

    #[test]
    fn searxng_top_ten() {
        let r = parse_searxng(&serde_json::from_str(include_str!("../tests/fixtures/searxng.json")).unwrap());
        assert_eq!(r.len(), 10);
        assert_eq!(r[0].engines, s(&["google", "bing"]));
        assert_eq!(r[0].snippet.chars().count(), 300);
        assert!(parse_searxng(&serde_json::json!({})).is_empty());
    }

    #[tokio::test]
    async fn serp_without_searxng_names_the_variable() {
        let err = serp(&reqwest::Client::new(), None, "k", "en", "us").await.unwrap_err();
        assert!(err.contains("SEARXNG_URL"));
    }

    #[tokio::test]
    #[ignore = "hits Google"]
    async fn live_ideas_and_trends() {
        let http = client().unwrap();
        let ideas = keyword_ideas(&http, &s(&["skincare"]), "en", "us").await.unwrap();
        assert!(!ideas["skincare"].is_empty());
        let interest = keyword_interest(&http, &s(&["skincare", "sunscreen"]), "EG", "").await.unwrap();
        assert_eq!(interest["skincare"], 100);
        assert!(interest.contains_key("sunscreen"));
    }
}
