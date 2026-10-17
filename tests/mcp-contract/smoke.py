#!/usr/bin/env python3
"""End-to-end MCP smoke test of the headless DeckCraft binary (WHIQ fork).

Usage:
    cargo build --release -p deckcraft-cli
    python3 tests/mcp-contract/smoke.py [path/to/deckcraft-cli] [--keep]

Starts `deckcraft-cli mcp` confined to a scratch folder, speaks MCP over stdio and checks the
contract end to end: handshake, annotations, a nine-layout deck with notes and overlays built in
one call, theme adoption from a .potx, PNG / PDF / outline export, a lossless in-place edit,
OLD_ renames, a job, a structured error and server_status. Prints one line per step and exits
non-zero on the first failure. Standard library only.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import threading
import queue
import time
import zipfile

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))


class Mcp:
    def __init__(self, binary, folder, extra=()):
        args = [binary, "mcp", "--automation-read-root", folder, "--automation-write-root", folder, "--author", "WHIQ smoke"] + list(extra)
        self.err_path = os.path.join(folder, "server-stderr.txt")
        self.err = open(self.err_path, "w", encoding="utf-8")
        self.p = subprocess.Popen(args, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=self.err, cwd=folder, text=True, encoding="utf-8")
        self.q = queue.Queue()
        threading.Thread(target=self._read, daemon=True).start()
        self.next = 1
        init = self.request("initialize", {"protocolVersion": "2025-06-18"})
        assert init["result"]["serverInfo"]["name"] == "deckcraft", init
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
        self.p.stdin.flush()

    def _read(self):
        for line in self.p.stdout:
            line = line.strip()
            if line:
                self.q.put(json.loads(line))

    def request(self, method, params, timeout=120):
        i = self.next
        self.next += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": i, "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        deadline = time.time() + timeout
        while True:
            v = self.q.get(timeout=max(0.1, deadline - time.time()))
            if v.get("id") == i:
                return v

    def call(self, tool, args=None):
        r = self.request("tools/call", {"name": tool, "arguments": args or {}})["result"]
        assert isinstance(r.get("structuredContent"), dict), ("structuredContent missing", tool, r)
        return r

    def ok(self, tool, args=None):
        r = self.call(tool, args)
        if r["isError"]:
            raise AssertionError(f"{tool} failed: {r['content'][0]['text']}")
        return r["structuredContent"]

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=60)
        self.err.close()
        return open(self.err_path, encoding="utf-8").read()


def step(name):
    print(f"ok  {name}", flush=True)


def make_png(m, folder, name):
    m.ok("new_presentation", {"theme": "Harbor"})
    path = os.path.join(folder, name)
    m.ok("render_slide", {"path": path, "scale": 0.5})
    m.ok("close_presentation")
    return path


def main():
    binary = os.path.abspath(next((a for a in sys.argv[1:] if not a.startswith("--")), os.path.join(ROOT, "target", "release", "deckcraft-cli")))
    keep = "--keep" in sys.argv
    folder = tempfile.mkdtemp(prefix="deckcraft-smoke-")
    print(f"binary {binary}\nfolder {folder}")
    m = Mcp(binary, folder)
    try:
        tools = m.request("tools/list", {})["result"]["tools"]
        names = {t["name"] for t in tools}
        for n in ["build_deck", "add_overlays", "adopt_template", "server_status", "job_status", "export", "save_presentation"]:
            assert n in names, n
        ro = {t["name"] for t in tools if t["annotations"]["readOnlyHint"]}
        assert "save_presentation" not in ro and "inspect_document" in ro
        assert all(t["annotations"]["destructiveHint"] for t in tools if t["name"] in ("save_presentation", "export"))
        step(f"handshake, {len(tools)} tools with annotations")

        photo = make_png(m, folder, "site-plan.png")
        deck = {
            "title": "Site Logistics Review",
            "subtitle": "Pilot week 3",
            "theme": "corporate",
            "slides": [
                {"layout": "bullets", "title": "Summary", "paragraphs": ["Where we are."], "bullets": ["**Scope** agreed", "  Phase 2 next", "Risks tracked"], "notes": "Open with the summary."},
                {"layout": "two-column", "title": "Options", "left": ["Option A", "Cheaper"], "right": ["Option B", "Faster"], "notes": "Compare."},
                {"layout": "table", "title": "Register", "table": [["ID", "Item", "Owner"]] + [[f"R-{i:03}", f"Item {i}", "Site"] for i in range(40)], "notes": "Long register: pages."},
                {"layout": "callout", "title": "Key message", "text": "Deliver phase 1 by March.", "notes": "Pause here."},
                {"layout": "kpi", "title": "Numbers", "kpis": [{"label": "Progress", "value": "62%"}, {"label": "Spend", "value": "$4.2M"}, {"label": "Days", "value": "118"}], "notes": "Three numbers."},
                {"layout": "section", "title": "Part two", "notes": "Section break."},
                {"layout": "quote", "title": "Client", "quote": "Safety first, then speed.", "attribution": "Site director", "notes": "Quote."},
                {"layout": "chart", "title": "Monthly output", "chart": {"kind": "bar", "categories": ["Jan", "Feb", "Mar"], "series": [{"name": "Plan", "values": [10, 20, 30]}, {"name": "Actual", "values": [8, 22, 27]}]}, "notes": "Chart."},
                {"layout": "image", "title": "Site plan", "image": {"path": "site-plan.png", "position": "full", "caption": "North at top"}, "notes": "Overlays.",
                 "overlays": [
                     {"kind": "rect", "x": 10, "y": 10, "w": 25, "h": 20, "color": "#2B6CB0", "label": "Storage"},
                     {"kind": "ellipse", "x": 55, "y": 15, "w": 30, "h": 40, "color": "#C53030", "dashed": True, "label": "Crane radius"},
                     {"kind": "line", "x": 5, "y": 90, "w": 95, "h": 60, "color": "#2F855A", "arrow": "both"},
                     {"kind": "path", "points": "5,50 20,45 35,55 50,48", "color": "#B7791F", "arrow": "end", "label": "Haul road"},
                     {"kind": "path", "points": [[60, 70], [80, 65], [90, 85], [65, 90]], "color": "#6B46C1", "filled": True, "label": "Compound"},
                     {"kind": "text", "x": 40, "y": 92, "w": 30, "h": 6, "color": "#1A202C", "label": "Gate 1"}]},
                {"layout": "image", "title": "Side picture", "image": {"path": "site-plan.png", "position": "right"}, "bullets": ["Text beside the picture"], "notes": "Right image."},
            ],
            "path": "review.pptx",
        }
        r = m.ok("build_deck", deck)
        assert r["slides"] >= 12, r
        doc = m.ok("inspect_document")
        # Table continuation pages ("Register (2/3)") carry no notes of their own.
        cont = lambda t: t.endswith(")") and "/" in t and not t.rsplit("(", 1)[-1].startswith("1/")
        without_notes = [s["index"] for s in doc["slides"] if s["index"] > 0 and not s.get("notes") and not cont(s["title"])]
        assert not without_notes, without_notes
        step(f"build_deck: {r['slides']} slides, nine layouts, notes on every content slide")

        z = zipfile.ZipFile(os.path.join(folder, "review.pptx"))
        xml = "".join(z.read(n).decode("utf-8", "replace") for n in z.namelist() if n.startswith("ppt/slides/slide"))
        for needle in ['prstDash val="dash"', 'tailEnd type="triangle"', 'headEnd type="triangle"', "<a:custGeom>", "<c:chart", "<a:tbl>"]:
            assert needle in xml, needle
        step("overlays: dashed, arrowed, freeform and filled shapes in the pptx")

        image_slide = next(s["index"] for s in doc["slides"] if s.get("title") == "Site plan")
        out = os.path.join(folder, "overlay.png")
        m.ok("render_slide", {"slide": image_slide, "path": out, "scale": 1})
        step(f"overlay regression slide rendered to {out}")

        m.ok("save_presentation", {"path": "template.potx"})
        ct = zipfile.ZipFile(os.path.join(folder, "template.potx")).read("[Content_Types].xml").decode()
        assert "presentationml.template.main+xml" in ct, ct
        r = m.ok("build_deck", {"template": "template.potx", "title": "From the template", "slides": [{"layout": "bullets", "title": "Hello", "bullets": ["one"], "notes": "n"}], "path": "from-template.pptx"})
        assert r["template"] is True, r
        step(".potx saved with the template content type; a deck built from it")

        m.ok("export", {"path": "slide.png", "scale": 0.5})
        m.ok("export", {"path": "deck.pdf"})
        m.ok("export", {"path": "outline.txt"})
        pdf = open(os.path.join(folder, "deck.pdf"), "rb").read()
        assert pdf[:4] == b"%PDF" and (b"/FontFile2" in pdf or b"/FontFile3" in pdf), "vector PDF with embedded fonts"
        if shutil.which("pdftotext"):
            txt = subprocess.run(["pdftotext", os.path.join(folder, "deck.pdf"), "-"], capture_output=True, text=True).stdout
            assert "From the template" in txt, txt
        assert "From the template" in open(os.path.join(folder, "outline.txt"), encoding="utf-8").read()
        step("export: PNG at scale 0.5, vector PDF with real text, outline")

        m.ok("open_presentation", {"path": "review.pptx"})
        sl = m.ok("inspect_slide", {"index": 1})
        title = next(s for s in sl["shapes"] if s.get("placeholder") in ("title", "ctrTitle") or s.get("name", "").startswith("Title"))
        m.ok("go_to_slide", {"index": 1})
        m.ok("set_text", {"id": title["id"], "text": "Summary (edited)"})
        r = m.ok("save_presentation", {})
        assert r.get("previous_files_renamed"), r
        before = set(zipfile.ZipFile(os.path.join(folder, "OLD_review.pptx")).namelist())
        after = set(zipfile.ZipFile(os.path.join(folder, "review.pptx")).namelist())
        assert before <= after, sorted(before - after)
        step("in-place edit kept every part; the previous file became OLD_review.pptx")

        r = m.ok("export", {"path": "all.png", "all": True, "scale": 0.2, "async": True})
        jid = r["job_id"]
        for _ in range(600):
            j = m.ok("job_status", {"job_id": jid})
            if j["state"] != "running":
                break
            time.sleep(0.1)
        assert j["state"] == "done", j
        step(f"job {jid}: {len(j['result']['files'])} PNGs")

        e = m.call("go_to_slide", {"index": 999})
        err = e["structuredContent"]["error"]
        assert e["isError"] and err["code"] == "out_of_range" and err["next_step"], err
        st = m.ok("server_status")
        step(f"structured error `{err['code']}`; server_status: {st['memory']['rss_mb']} MB, {len(st['documents'])} documents")
    finally:
        stderr = m.close()
        if not keep:
            shutil.rmtree(folder, ignore_errors=True)
        else:
            print(f"kept {folder}")
    if "panicked" in stderr:
        print(stderr)
        sys.exit(1)
    print("smoke test passed")


if __name__ == "__main__":
    main()
