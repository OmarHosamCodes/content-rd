# MCP tool reference

Endpoint: `/mcp` (streamable HTTP, bearer `MCP_TOKEN`). Every failure comes back as a tool error with a readable message.

## `find_reels(topic, platform?, limit?)` — `{url, plays, user, caption, likes?, comments?}`

Highest-exposure short videos for a topic, most plays first.

- `topic` (string): search topic or hashtag
- `platform` ("instagram" \| "tiktok" \| "youtube", default instagram)
- `limit` (1-50, default 10): up to 12 on Instagram, 10 on TikTok, ~20 on YouTube

Instagram and TikTok run on the server; YouTube search reads the results page with no key. TikTok needs `SEARXNG_URL` (`site:tiktok.com <topic>`).

## `get_comments(url, max_comments?, cursor?, contains?)` — `{comments, scanned, next_cursor}`

Paged comments of a reel or video URL from Instagram, YouTube or TikTok.

- `url` (string, alias `reel_url`): Instagram reel, YouTube video/Short, or `https://www.tiktok.com/@user/video/<id>`
- `max_comments` (default 300): up to ~300 top-level comments per call
- `cursor` (default ""): `next_cursor` from the previous call to keep scrolling
- `contains` (strings): keep only comments containing any of these words, case-insensitive

Returns `comments[{user, text, likes, replies}]`, how many were scanned, and `next_cursor` ("" when done). Page until ~1000 per video; TikTok stops at ~300 without a login.

## `keyword_ideas(seeds, lang?, country?)` — Google autocomplete expansion

- `seeds` (strings): seed keywords
- `lang` (default "en"), `country` (default "us")

## `keyword_interest(keywords, geo?, anchor?)` — Google Trends demand

- `keywords` (strings): candidates to score
- `geo` (default ""): country code like EG or US; empty = worldwide
- `anchor` (default first keyword): well-known term demand is relative to

Interest is relative demand, not monthly volume.

## `serp(keyword, lang?, country?)` — top web results via private SearXNG

- `keyword` (string), `lang` (default "en"), `country` (default "us")

## `board_add_comments(board, pattern, comments[{user, text, likes, reel_url}], reel_url?)`

Pin comments under a pattern label; deduped by `(user, text)`.

## `board_add_gap(board, keyword, interest, scores{}, verdict, top_results[{title, url}])`

Save a scored keyword; upserts by keyword; board ranks gaps by total score.

## Prompt: `content_rd(industry, market?)`

Runs the full workflow: pick one board, mine comments on Instagram/TikTok/YouTube, hunt patterns with `contains`, save gaps with `board_add_gap`, finish with the board URL and top 3 recommendations.

## Rate-limit note

Call `find_reels` and `get_comments` one at a time, never in parallel — parallel calls get rate-limited. On Instagram, a failed public call retries once through the optional local bridge; if the bridge is offline, everything else keeps working.
