# Content R&D MCP

One prompt -> Instagram comment patterns + ranked search gaps on a board page.

## Setup (once)

```sh
python3 -m venv --system-site-packages .venv
.venv/bin/pip install playwright "mcp>=2" httpx     # uses your installed Google Chrome
.venv/bin/python server.py login                    # sign in to Instagram, close the window
```

Use a secondary Instagram account. Automated reading can get an account flagged.

## Run

```sh
.venv/bin/python server.py
```

A Chrome window opens on first use and stays open; leave it alone.

- MCP endpoint: `http://localhost:8787/mcp` (streamable HTTP)
- Boards: `http://localhost:8787/b/<board-name>`, list at `/`
- Env: `PORT`, `BASE_URL` (public address if you put it behind a tunnel or domain), `HEADLESS=1` (hide Chrome; Google then blocks `google_serp` with a captcha), `CHROME` (browser binary path)

## Connect an agent

```sh
claude mcp add --transport http content-rd http://localhost:8787/mcp
```

Any other agent: add an HTTP MCP server with that URL.

## Use

> Run Content R&D for skincare in Egypt (Arabic). Board: skincare-eg

or the `content_rd` MCP prompt. The agent gets the full workflow from the server instructions.

## Tools

| Tool | Does |
| --- | --- |
| `find_reels` | highest-exposure reels for a topic |
| `get_comments` | paged comments, optional keyword filter for pattern hunting |
| `board_add_comments` | pin comments under a pattern, uniform cards |
| `keyword_ideas` | Google autocomplete expansion |
| `keyword_interest` | Google Trends demand, relative to an anchor keyword |
| `google_serp` | top 10 Google results to judge |
| `board_add_gap` | save a scored keyword, board ranks by total |

Test: `.venv/bin/python test_server.py`
