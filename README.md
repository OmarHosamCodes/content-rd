# Content R&D MCP

One prompt -> comment patterns from Instagram, TikTok and YouTube + ranked search gaps on a board page.

- Server (Rust, on Railway): `https://content-rd-production.up.railway.app`
- MCP endpoint: `/mcp` (streamable HTTP, bearer token)
- Boards: `/b/<board-name>` (public to anyone with the link)

The token and server URL are in `.env` (not in git) and in the Railway service variables.

## Connect an agent

```sh
claude mcp add --transport http content-rd https://content-rd-production.up.railway.app/mcp --header "Authorization: Bearer <MCP_TOKEN>"
```

Any other agent: add an HTTP MCP server with that URL and header.

## Use

> Run Content R&D for skincare in Egypt (Arabic). Board: skincare-eg

or the `content_rd` MCP prompt. The agent gets the full workflow from the server instructions.

## TikTok and YouTube

Read logged out, nothing to set up. TikTok videos are found through SearXNG (`site:tiktok.com <topic>`, TikTok's own search needs a login), then each video page gives its plays. Logged out, TikTok stops after ~300 top-level comments per video. YouTube search reads the results page; comments come from the web client's own API (no key). If YouTube comments start failing with HTTP 400, bump `YT_CLIENT` in `src/video.rs`.

## Instagram

The server reads public reels and comments itself, logged out, the way instagram.com serves visitors without an account. Nothing to set up.

If that fails (rate limit, blocked IP, Instagram changed its query ids), each call retries once through the optional bridge below.

### Bridge (optional fallback, your computer)

Chrome on your own machine, logged in to Instagram, so no login or cookie is ever sent to the server.

```sh
env $(cat .env) cargo run --release -- bridge
```

A Chrome window opens. Log in to Instagram in it once (use a secondary account) and leave it open while a run is in progress. Stop with Ctrl-C. Without it, a failed Instagram call reports both the public error and "bridge is offline"; everything else keeps working.

Bridge env: `SERVER_URL`, `MCP_TOKEN` (required), `CHROME` (browser path), `BRIDGE_PROFILE` (profile folder, default `~/.local/share/content-rd/chrome`).

When public calls start failing with "doc_id may have rotated", re-harvest the two query ids in `src/instagram.rs`.

## Tools

| Tool | Does | Runs on |
| --- | --- | --- |
| `find_reels` | highest-exposure videos for a topic on instagram, tiktok or youtube, by plays | server, bridge fallback for Instagram |
| `get_comments` | paged comments of a reel or video URL from any of the three, optional keyword filter for pattern hunting | server, bridge fallback for Instagram |
| `board_add_comments` | pin comments under a pattern, uniform cards | server |
| `keyword_ideas` | Google autocomplete expansion | server |
| `keyword_interest` | Google Trends demand, relative to an anchor keyword | server |
| `serp` | top web results via a private SearXNG | server |
| `board_add_gap` | save a scored keyword, board ranks by total | server |

## Server env

| Variable | Meaning |
| --- | --- |
| `MCP_TOKEN` | required bearer token for `/mcp` and `/bridge/*` |
| `DATA_DIR` | where boards are stored (`/data` volume on Railway) |
| `SEARXNG_URL` | private SearXNG service, `http://searxng.railway.internal:8080` |
| `BASE_URL` | public address; defaults to the Railway domain |
| `PORT` | default 8787 |

## Develop

```sh
cargo test                       # unit tests
cargo test live -- --ignored     # hits Google autocomplete, Trends, public Instagram, TikTok and YouTube
MCP_TOKEN=dev cargo run          # local server on :8787
railway up --service content-rd  # deploy the server
railway up searxng --path-as-root --service searxng
```
