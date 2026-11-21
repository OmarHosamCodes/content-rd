# Contributing

## Develop

```sh
cargo test                       # unit tests
cargo test live -- --ignored     # hits live Instagram, TikTok, YouTube, autocomplete, Trends
MCP_TOKEN=dev cargo run          # local server on :8787
railway up --service content-rd  # deploy the server
railway up searxng --path-as-root --service searxng
```

`.env` holds local dev values and is not in git. No secrets, tokens or API keys may ever be committed; fixtures are anonymized.

## Adding a platform

Follow the existing `src/video.rs` / `src/instagram.rs` pattern:

1. URL id parser (`youtube_id`, `tiktok_id`, `instagram` shortcode helpers)
2. Page parser for stats + caption (`parse_tiktok_page`, `parse_public_reels`, `parse_youtube_search`)
3. Comments parser + paged fetcher (`parse_youtube_comments`/`youtube_comments`, etc.)
4. Register a `Platform` variant and its dispatch in `src/tools.rs` (`find_reels` / `get_comments`)
5. Add fixtures under `tests/fixtures/`, unit tests, and an ignored live test hitting the real platform

Callers: every tool and route that takes a platform. Keep error handling — no `unwrap`/`expect` on network or user data.

## Known-limit shortcuts

A shortcut with a known limit gets a one-line comment:

```rust
// shortcut: <the limit>, <when to upgrade>
```

Never cut: validation at trust boundaries, error handling that prevents data loss, the board name rule, or anonymized fixtures.

## Commit style

Conventional commits (`feat:`, `fix:`), end with:

```
Co-Authored-By: Claude Code <noreply@anthropic.com>
```
