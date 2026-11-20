"""Content R&D MCP server: Instagram comment mining + search-gap research -> board page.

Run:    .venv/bin/python server.py            (MCP at /mcp, boards at /b/<name>)
Login:  .venv/bin/python server.py login      (once, opens Chrome to sign in to Instagram)
"""
import asyncio, html, json, os, re, shutil, sys
from pathlib import Path
from statistics import mean
from urllib.parse import quote

import httpx
from mcp.server.mcpserver import MCPServer
from mcp.server.mcpserver.exceptions import ToolError  # only ToolError text reaches the agent
from playwright.async_api import async_playwright
from starlette.responses import HTMLResponse

ROOT = Path(__file__).parent
BOARDS = ROOT / "boards"
PROFILE = ROOT / "chrome-profile"
PORT = int(os.environ.get("PORT", 8787))
BASE_URL = os.environ.get("BASE_URL", f"http://localhost:{PORT}").rstrip("/")
HEADLESS = os.environ.get("HEADLESS") == "1"  # visible Chrome by default: Google captchas headless browsers
CHROME = os.environ.get("CHROME") or shutil.which("google-chrome") or shutil.which("chromium")
UA = {"User-Agent": "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0 Safari/537.36"}
LOGIN_HELP = "Instagram session missing or blocked. Stop the server, run `python server.py login`, sign in, close the window, restart."

WORKFLOW = """Content R&D workflow. Pick one board name (lowercase-with-dashes) and reuse it in every board_* call.

A) Comment mining
1. Choose 3-5 topics for the industry. For each: find_reels(topic) -> highest-exposure reels.
2. For the top reels: get_comments(reel_url). Page with next_cursor until ~1000 comments are scanned per reel.
3. Keep only comments that reveal a real pain, question, objection, desire or misconception. Expect ~5 per 1000.
4. For each insight, hunt for a pattern: call get_comments on other reels with contains=[keywords of that insight].
5. Save with board_add_comments(board, pattern, comments) - one call per pattern, pattern = short label of the insight.

B) Search gaps
1. Brainstorm 20+ seed keywords, expand with keyword_ideas.
2. keyword_interest on the candidates. If interest is low, try new keywords until some score high.
3. For each high-interest keyword: google_serp(keyword). Judge the top results: weak, outdated or unrelated = gap.
4. Save each promising keyword with board_add_gap, scoring 1-5 on: demand, serp_weakness, relevance, ease.
5. Stop at 5+ saved gaps. The board ranks them by total score.

Finish by giving the user the board URL and the top 3 recommendations."""

mcp = MCPServer("content-rd", instructions=WORKFLOW)


@mcp.prompt()
def content_rd(industry: str, market: str = "") -> str:
    """Run the full Content R&D workflow for an industry."""
    return f"Run Content R&D for the industry: {industry}. Market/language: {market or 'infer from the industry'}.\n\n{WORKFLOW}"


# ---------- browser ----------
_ctx = None
_lock = asyncio.Lock()


async def browser(headless=HEADLESS):
    global _ctx
    async with _lock:
        if not _ctx:
            pw = await async_playwright().start()
            _ctx = await pw.chromium.launch_persistent_context(
                str(PROFILE), executable_path=CHROME, headless=headless,
                args=["--disable-blink-features=AutomationControlled"])
        return _ctx


# ---------- instagram ----------
def shortcode_to_pk(code: str) -> int:
    abc = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    n = 0
    for ch in code:
        n = n * 64 + abc.index(ch)
    return n


def walk_media(o, out: dict):
    """Collect every post-like dict from any Instagram JSON, whatever its shape."""
    if isinstance(o, dict):
        if isinstance(o.get("code"), str) and "comment_count" in o:
            cap = o.get("caption")
            out[o["code"]] = {
                "url": f"https://www.instagram.com/reel/{o['code']}/",
                "plays": o.get("play_count") or o.get("ig_play_count") or o.get("view_count") or 0,
                "likes": o.get("like_count") or 0,
                "comments": o.get("comment_count") or 0,
                "user": (o.get("user") or o.get("owner") or {}).get("username", ""),
                "caption": ((cap.get("text") if isinstance(cap, dict) else cap) or "")[:160],
            }
        for v in o.values():
            walk_media(v, out)
    elif isinstance(o, list):
        for v in o:
            walk_media(v, out)


def _loads(text: str):
    return json.loads(text[text.index("{"):])  # strips anti-JSON-hijack prefixes like )]}' and for(;;);


@mcp.tool()
async def find_reels(topic: str, limit: int = 10) -> list[dict]:
    """Find the highest-exposure Instagram posts/reels for a topic, sorted by plays then comment count."""
    page = await (await browser()).new_page()
    found: dict = {}

    async def on_response(r):
        if "instagram.com" in r.url and r.request.resource_type in ("xhr", "fetch"):
            try:
                walk_media(_loads(await r.text()), found)
            except Exception:
                pass  # non-JSON response

    page.on("response", on_response)
    try:
        await page.goto(f"https://www.instagram.com/explore/search/keyword/?q={quote(topic)}", wait_until="domcontentloaded")
        if "/accounts/login" in page.url:
            raise ToolError(LOGIN_HELP)
        for _ in range(4):
            await page.wait_for_timeout(1500)
            await page.mouse.wheel(0, 4000)
        for s in await page.eval_on_selector_all('script[type="application/json"]', "els => els.map(e => e.textContent)"):
            try:
                walk_media(json.loads(s), found)
            except ValueError:
                pass
    finally:
        await page.close()
    if not found:
        raise ToolError(f"No posts found for {topic!r}. Try a hashtag-style topic, or: {LOGIN_HELP}")
    return sorted(found.values(), key=lambda m: (m["plays"], m["comments"]), reverse=True)[:limit]


@mcp.tool()
async def get_comments(reel_url: str, max_comments: int = 300, cursor: str = "", contains: list[str] = []) -> dict:
    """Read comments of a reel/post. Returns {comments:[{user,text,likes,replies}], scanned, next_cursor}.
    Pass next_cursor back as cursor to keep scrolling. contains = keep only comments containing any of these words.
    Comments under 3 words are dropped as noise."""
    m = re.search(r"instagram\.com/(?:[\w.]+/)?(?:reels?|p|tv)/([\w-]+)", reel_url)
    if not m:
        raise ToolError("reel_url must look like https://www.instagram.com/reel/<code>/")
    pk = shortcode_to_pk(m[1])
    page = await (await browser()).new_page()
    kept, scanned = [], 0
    try:
        await page.goto("https://www.instagram.com/", wait_until="domcontentloaded")
        while scanned < max_comments:
            path = f"/api/v1/media/{pk}/comments/?can_support_threading=true&permalink_enabled=false"
            if cursor:
                path += "&min_id=" + quote(cursor)
            status, text = await page.evaluate(
                """async p => { const r = await fetch(p, {headers: {'X-IG-App-ID': '936619743392459'}, credentials: 'include'});
                               return [r.status, await r.text()] }""", path)
            try:
                data = json.loads(text)
            except ValueError:
                raise ToolError(f"{LOGIN_HELP} (HTTP {status})")
            if status != 200:
                raise ToolError(f"Instagram said {status}: {data.get('message', '')}. {LOGIN_HELP}")
            batch = data.get("comments") or []
            scanned += len(batch)
            for c in batch:
                t = c.get("text") or ""
                if len(t.split()) >= 3 and (not contains or any(k.lower() in t.lower() for k in contains)):
                    kept.append({"user": (c.get("user") or {}).get("username", ""), "text": t,
                                 "likes": c.get("comment_like_count", 0), "replies": c.get("child_comment_count", 0)})
            cursor = data.get("next_min_id") or ""
            if not cursor or not batch:
                break
            await page.wait_for_timeout(1000)  # stay gentle, the account gets flagged otherwise
    finally:
        await page.close()
    return {"comments": kept, "scanned": scanned, "next_cursor": cursor}


# ---------- search gaps ----------
@mcp.tool()
async def keyword_ideas(seeds: list[str], lang: str = "en", country: str = "us") -> dict[str, list[str]]:
    """Expand seed keywords with what people actually type (Google autocomplete). lang/country like ar/eg."""
    async with httpx.AsyncClient(headers=UA, timeout=20) as c:
        out = {}
        for s in seeds:
            r = await c.get("https://suggestqueries.google.com/complete/search",
                            params={"client": "firefox", "hl": lang, "gl": country, "q": s})
            r.raise_for_status()
            out[s] = r.json()[1]
        return out


@mcp.tool()
async def keyword_interest(keywords: list[str], geo: str = "", anchor: str = "") -> dict[str, int]:
    """Search demand over the last 12 months from Google Trends, relative to the anchor keyword (anchor = 100).
    geo = country code like EG or US, empty = worldwide. anchor defaults to the first keyword; use a well-known term.
    These are relative numbers, not monthly search volumes."""
    anchor = anchor or keywords[0]
    rest = [k for k in dict.fromkeys(keywords) if k != anchor]
    t = "https://trends.google.com/trends"
    out = {anchor: 100}
    async with httpx.AsyncClient(headers=UA, timeout=30, follow_redirects=True) as c:
        await c.get(f"{t}/", params={"geo": geo})  # sets the cookie the API requires
        for i in range(0, len(rest), 4):  # Trends compares at most 5 terms, one slot is the anchor
            kws = [anchor] + rest[i:i + 4]
            req = {"comparisonItem": [{"keyword": k, "geo": geo, "time": "today 12-m"} for k in kws], "category": 0, "property": ""}
            r = await c.get(f"{t}/api/explore", params={"hl": "en-US", "tz": 0, "req": json.dumps(req)})
            r.raise_for_status()
            w = next(w for w in _loads(r.text)["widgets"] if w["id"] == "TIMESERIES")
            r = await c.get(f"{t}/api/widgetdata/multiline", params={"hl": "en-US", "tz": 0, "req": json.dumps(w["request"]), "token": w["token"]})
            r.raise_for_status()
            rows = _loads(r.text)["default"]["timelineData"]
            means = [mean(row["value"][j] for row in rows) if rows else 0 for j in range(len(kws))]
            if not means[0]:
                raise ToolError(f"Anchor {anchor!r} has no search interest in {geo or 'worldwide'}; pick a more popular anchor.")
            out.update({k: round(v / means[0] * 100) for k, v in zip(kws[1:], means[1:])})
            await asyncio.sleep(1)
    return out


@mcp.tool()
async def google_serp(keyword: str, lang: str = "en", country: str = "us") -> list[dict]:
    """Top Google results for a keyword: [{title, site, url, snippet}]. Judge them: weak/unrelated results = content gap."""
    page = await (await browser()).new_page()
    try:
        await page.goto(f"https://www.google.com/search?q={quote(keyword)}&hl={quote(lang)}&gl={quote(country)}", wait_until="domcontentloaded")
        if "/sorry/" in page.url:
            raise ToolError("Google showed a captcha. Solve it in the Chrome window, then retry (headless mode cannot pass it).")
        await page.wait_for_selector("a h3", timeout=15000)
        return await page.evaluate("""() => [...document.querySelectorAll('a:has(h3)')].slice(0, 10).map(a => {
            const title = a.querySelector('h3').innerText;
            let box = a;  // widest ancestor that still holds only this result
            while (box.parentElement && box.parentElement.querySelectorAll('a h3').length === 1) box = box.parentElement;
            const site = a.querySelector('cite')?.innerText || '';  // Google hides the real URL behind a redirect
            return {title, site, url: a.href, snippet: box.innerText.replace(title, '').replace(site, '').replace(/\\s+/g, ' ').trim().slice(0, 300)};
        })""")
    finally:
        await page.close()


# ---------- board ----------
def _path(board: str) -> Path:
    if not re.fullmatch(r"[a-z0-9][a-z0-9-]{0,63}", board):
        raise ToolError("board name must be lowercase letters, digits and dashes")
    return BOARDS / f"{board}.json"


def _load(board: str) -> dict:
    p = _path(board)
    return json.loads(p.read_text()) if p.exists() else {"comments": [], "gaps": []}


def _save(board: str, data: dict) -> str:
    BOARDS.mkdir(exist_ok=True)
    _path(board).write_text(json.dumps(data, ensure_ascii=False, indent=1))
    return f"{BASE_URL}/b/{board}"


@mcp.tool()
def board_add_comments(board: str, pattern: str, comments: list[dict], reel_url: str = "") -> str:
    """Pin valuable comments to the board under a pattern label. comments = [{user, text, likes}]. Returns the board URL."""
    data = _load(board)
    seen = {(c["user"], c["text"]) for c in data["comments"]}
    for c in comments:
        card = {"pattern": pattern, "user": str(c.get("user", "")), "text": str(c["text"]),
                "likes": int(c.get("likes") or 0), "reel_url": str(c.get("reel_url") or reel_url)}
        if (card["user"], card["text"]) not in seen:
            seen.add((card["user"], card["text"]))
            data["comments"].append(card)
    return _save(board, data)


@mcp.tool()
def board_add_gap(board: str, keyword: str, interest: int, scores: dict[str, int], verdict: str, top_results: list[dict] = []) -> str:
    """Save a search-gap keyword. scores = 1-5 per dimension, e.g. {demand, serp_weakness, relevance, ease}.
    verdict = one sentence on why the current results are weak. top_results = google_serp output. Returns the board URL."""
    data = _load(board)
    data["gaps"] = [g for g in data["gaps"] if g["keyword"] != keyword] + [{
        "keyword": keyword, "interest": int(interest), "scores": {str(k): int(v) for k, v in scores.items()},
        "verdict": verdict, "results": [{"title": str(r.get("title", "")), "url": str(r.get("url", ""))} for r in top_results[:10]]}]
    return _save(board, data)


CSS = """
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
"""


def _link(url: str, label: str) -> str:
    e = html.escape
    return f'<a href="{e(url)}" target="_blank" rel="noopener noreferrer">{e(label)}</a>' if url.startswith("https://") else e(label)


def render(board: str, data: dict) -> str:
    e = html.escape
    out = [f'<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">'
           f"<title>{e(board)} - Content R&amp;D</title><style>{CSS}</style><main><h1>{e(board)}</h1>"
           f'<p class="sub">{len(data["comments"])} comments, {len(data["gaps"])} search gaps</p>']
    if data["gaps"]:
        gaps = sorted(data["gaps"], key=lambda g: sum(g["scores"].values()), reverse=True)
        dims = list(dict.fromkeys(d for g in gaps for d in g["scores"]))
        out.append('<h2>Search gaps</h2><p class="sub">Ranked by total score. Interest is relative Google Trends demand, not monthly volume.</p>'
                   '<div class="wrap"><table><tr><th>#</th><th>Keyword</th><th class="n">Interest</th>'
                   + "".join(f'<th class="n">{e(d.replace("_", " "))}</th>' for d in dims) + '<th class="n">Total</th></tr>')
        for i, g in enumerate(gaps, 1):
            res = "".join(f"<li>{_link(r['url'], r['title'] or r['url'])}</li>" for r in g["results"])
            out.append(f'<tr><td>{i}</td><td><strong dir="auto">{e(g["keyword"])}</strong><div dir="auto">{e(g["verdict"])}</div>'
                       + (f"<details><summary>Current top results</summary><ol>{res}</ol></details>" if res else "")
                       + f'</td><td class="n">{g["interest"]}</td>'
                       + "".join(f'<td class="n">{g["scores"].get(d, "")}</td>' for d in dims)
                       + f'<td class="n total">{sum(g["scores"].values())}</td></tr>')
        out.append("</table></div>")
    if data["comments"]:
        out.append("<h2>Comment patterns</h2>")
        groups: dict = {}
        for c in data["comments"]:
            groups.setdefault(c["pattern"], []).append(c)
        for pattern, cards in sorted(groups.items(), key=lambda kv: -len(kv[1])):
            out.append(f'<h3 dir="auto">{e(pattern)} ({len(cards)})</h3><div class="grid">')
            for c in sorted(cards, key=lambda c: -c["likes"]):
                out.append(f'<article class="card"><p dir="auto" title="{e(c["text"])}">{e(c["text"])}</p>'
                           f'<footer><span>@{e(c["user"])}</span><span>{c["likes"]} likes {_link(c["reel_url"], "reel")}</span></footer></article>')
            out.append("</div>")
    if not data["comments"] and not data["gaps"]:
        out.append("<p>Nothing here yet.</p>")
    return "".join(out) + "</main></html>"


@mcp.custom_route("/b/{board}", methods=["GET"])
async def board_page(request):
    board = request.path_params["board"]
    try:
        exists = _path(board).exists()
    except ToolError:
        exists = False
    if not exists:
        return HTMLResponse("Board not found", 404)
    return HTMLResponse(render(board, _load(board)))


@mcp.custom_route("/", methods=["GET"])
async def index(request):
    names = sorted(p.stem for p in BOARDS.glob("*.json"))
    items = "".join(f'<li><a href="/b/{n}">{n}</a></li>' for n in names) or "<li>No boards yet.</li>"
    return HTMLResponse(f'<!doctype html><meta charset="utf-8"><title>Content R&amp;D boards</title><style>{CSS}</style><main><h1>Boards</h1><ul>{items}</ul></main>')


async def login():
    ctx = await browser(headless=False)
    page = ctx.pages[0] if ctx.pages else await ctx.new_page()
    await page.goto("https://www.instagram.com/accounts/login/")
    print("Sign in to Instagram, then close the Chrome window.")
    await ctx.wait_for_event("close", timeout=0)


if __name__ == "__main__":
    if sys.argv[1:] == ["login"]:
        asyncio.run(login())
    else:
        print(f"MCP: {BASE_URL}/mcp   Boards: {BASE_URL}/", file=sys.stderr)
        mcp.run("streamable-http", port=PORT)
