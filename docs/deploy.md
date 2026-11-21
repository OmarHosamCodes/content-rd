# Deploy

The server is a Rust binary (axum + rmcp) shipped as a Docker image and run on Railway.

## 1. Deploy SearXNG (TikTok search + `serp`)

TikTok's own search needs a login, so videos are found through a private SearXNG.

```sh
railway up searxng --path-as-root --service searxng
```

Note `SEARXNG_URL` on the content-rd service, e.g.
`http://searxng.railway.internal:8080`.

## 2. Deploy the content-rd server

```sh
railway up --service content-rd
```

The Dockerfile builds with `cargo build --release --locked` and runs the binary as root so a mounted volume is writable.

## 3. Set service variables in Railway

| Variable | Meaning | Example |
|---|---|---|
| `MCP_TOKEN` | required bearer token for `/mcp` and `/bridge/*` | generate one, keep it secret |
| `DATA_DIR` | where boards are stored | `/data` |
| `SEARXNG_URL` | private SearXNG service | `http://searxng.railway.internal:8080` |
| `BASE_URL` | public address; defaults to the Railway domain | `https://content-rd-production.up.railway.app` |
| `PORT` | default 8787 | `8787` |

`MCP_TOKEN` is required; the server refuses to start without it.

## 4. Persist boards across deploys

Boards are JSON files under `DATA_DIR` (`/data` volume on Railway). Mount a Railway volume at `/data` on the content-rd service so boards survive redeploys.

## 5. Connect an agent

```sh
claude mcp add --transport http content-rd https://content-rd-production.up.railway.app/mcp --header "Authorization: Bearer <MCP_TOKEN>"
```

MCP endpoint: `/mcp` (streamable HTTP). Public board pages: `/b/<board-name>`.

## Local run

```sh
MCP_TOKEN=dev cargo run          # server on :8787
cargo test                       # unit tests
cargo test live -- --ignored     # hits live Instagram, TikTok, YouTube, autocomplete, Trends
```
