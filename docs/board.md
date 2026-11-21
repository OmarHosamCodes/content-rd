# Boards

A board is a JSON file under `DATA_DIR`, named `<board-name>.json`, and served as a public read-only page at `/b/<board-name>`.

## Board name

`[a-z0-9][a-z0-9-]{0,63}` — lowercase letters, digits, dashes, must start with a letter or digit, max 64 chars. This is the only thing that ever becomes a file name, so `../x`, `a.json`, `A`, `-a`, `a b` are rejected everywhere (every tool and the `/b/<name>` route).

## Board JSON

```json
{
  "comments": [
    { "pattern": "string", "user": "string", "text": "string", "likes": 0, "reel_url": "string" }
  ],
  "gaps": [
    { "keyword": "string", "interest": 0, "scores": { "demand": 0 }, "verdict": "string", "results": [{ "title": "string", "url": "string" }] }
  ]
}
```

Example: `boards/demo-skincare.json` (anonymized).

## How the page renders

- Search gaps ranked by total score (sum of `scores`), highest first. Interest is relative Google Trends demand, not monthly volume. Each gap shows its verdict and current top results; only `https://` links are clickable, everything else is plain text.
- Comment patterns grouped by `pattern`, largest group first; within a group, most likes first. User names and comment text use `dir="auto"`; nothing a user typed is ever rendered as HTML.

## Writing to a board

`board_add_comments(board, pattern, comments[{user, text, likes, reel_url}], reel_url?)` — one call per pattern. Comments are deduplicated by `(user, text)` so re-saving the same insight never duplicates cards. A comment's `reel_url` falls back to the board-level `reel_url`.

`board_add_gap(board, keyword, interest, scores{1-5 per dimension}, verdict, top_results[{title, url}])` — upserts by keyword: saving the same keyword again replaces its scores and verdict. The board re-ranks on every save.

Writes are load-modify-save under a lock, saved to a temp file then atomically renamed, so parallel tool calls and crashes never lose or half-write a board.
