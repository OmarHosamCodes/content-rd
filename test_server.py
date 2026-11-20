"""Offline self-check: .venv/bin/python test_server.py"""
import server as s

assert s.shortcode_to_pk("BA") == 64 and s.shortcode_to_pk("_") == 63

found = {}
s.walk_media({"data": {"items": [{"media": {"code": "Cabc", "comment_count": 9, "play_count": 5000,
                                              "caption": {"text": "hi"}, "user": {"username": "u"}}},
                                  {"code": "notmedia"}]}}, found)
assert list(found) == ["Cabc"] and found["Cabc"]["plays"] == 5000 and found["Cabc"]["caption"] == "hi"

assert s._loads(")]}'\n{\"a\": 1}") == {"a": 1}

for bad in ("../etc/passwd", "A", "", "a/b"):
    try:
        s._path(bad)
        raise SystemExit(f"accepted bad board name {bad!r}")
    except s.ToolError:
        pass

page = s.render("t", {"comments": [{"pattern": "<b>p", "user": "x", "text": "<script>alert(1)</script>", "likes": 3,
                                     "reel_url": "javascript:alert(1)"}],
                      "gaps": [{"keyword": "kw-b", "interest": 1, "scores": {"demand": 1}, "verdict": "v", "results": []},
                               {"keyword": "kw-a", "interest": 9, "scores": {"demand": 5}, "verdict": "v",
                                "results": [{"title": "T", "url": "https://e.com"}]}]})
assert "<script>alert" not in page and "javascript:" not in page and "<b>p" not in page
assert page.index("kw-a") < page.index("kw-b")  # ranked by total score
print("ok")
