from __future__ import annotations

import html
import unicodedata
from dataclasses import dataclass
from pathlib import Path

from .brand import PALETTE
from .model import clean


def cell_width(char: str) -> int:
    if unicodedata.combining(char):
        return 0
    return 2 if unicodedata.east_asian_width(char) in ("W", "F") else 1


def width(text: str) -> int:
    return sum(cell_width(c) for c in text)


def clip(text: str, maximum: int, ellipsis: bool = True) -> str:
    if maximum <= 0:
        return ""
    if width(text) <= maximum:
        return text
    result, used = [], 0
    budget = maximum - (1 if ellipsis else 0)
    for c in text:
        w = cell_width(c)
        if used + w > budget:
            break
        result.append(c)
        used += w
    return "".join(result) + ("…" if ellipsis else "")


def wrap(text: str, maximum: int) -> list[str]:
    result: list[str] = []
    if maximum <= 0:
        return result
    for paragraph in clean(text).expandtabs(4).splitlines():
        words = paragraph.split()
        current = ""
        for word in words:
            if current and width(current + " " + word) > maximum:
                result.append(current)
                current = ""
            while width(word) > maximum:
                piece = clip(word, maximum, ellipsis=False)
                result.append(piece)
                word = word[len(piece):]
            if word:
                current = f"{current} {word}".strip()
        result.append(current)
    return result or [""]


@dataclass
class Cell:
    char: str = " "
    style: str = "text"
    selected: bool = False
    bold: bool = False
    background: str = ""


class Screen:
    def __init__(self, cols: int, rows: int):
        self.cols, self.rows = max(1, cols), max(1, rows)
        self.cells = [[Cell() for _ in range(self.cols)] for _ in range(self.rows)]
        # Clickable regions: (y0, y1, x0, x1, action); later regions win.
        self.hits: list[tuple[int, int, int, int, tuple]] = []

    def hit(self, y: int, x: int, w: int, action: tuple, h: int = 1):
        if w > 0 and h > 0:
            self.hits.append((y, y + h, x, x + w, action))

    def hit_at(self, y: int, x: int) -> tuple | None:
        for y0, y1, x0, x1, action in reversed(self.hits):
            if y0 <= y < y1 and x0 <= x < x1:
                return action
        return None

    def put(self, y: int, x: int, text: object, style: str = "text", selected: bool = False,
            bold: bool = False, limit: int | None = None):
        if not 0 <= y < self.rows or x >= self.cols:
            return
        value = clean(text).replace("\n", " ").replace("\t", " ")
        value = clip(value, min(self.cols - max(x, 0), limit if limit is not None else self.cols), ellipsis=True)
        for c in value:
            w = cell_width(c)
            if w == 0:
                if 0 < x <= self.cols:
                    self.cells[y][x - 1].char += c
                continue
            if x >= 0 and x + w <= self.cols:
                self.cells[y][x] = Cell(c, style, selected, bold)
                if w == 2:
                    self.cells[y][x + 1] = Cell("", style, selected, bold)
            x += w

    def spans(self, y: int, x: int, parts, selected: bool = False, limit: int | None = None) -> int:
        """Put (text, style[, bold]) parts side by side; return the end column."""
        end = self.cols if limit is None else x + limit
        for part in parts:
            text, style, bold = (*part, False)[:3]
            if x >= end:
                break
            self.put(y, x, text, style, selected, bold, limit=end - x)
            x += width(clip(clean(text), end - x))
        return x

    def fill(self, y: int, x: int, count: int, selected: bool = False, style: str = "text"):
        self.put(y, x, " " * max(0, count), style, selected)

    def shade(self, y: int, x: int, count: int, background: str):
        if 0 <= y < self.rows:
            for cell in self.cells[y][max(0, x):max(0, min(self.cols, x + count))]:
                cell.background = background

    def rule(self, y: int, x: int = 2, count: int | None = None):
        self.put(y, x, "─" * max(0, count if count is not None else self.cols - x - 2), "faint")

    def runs(self, y: int):
        start, value, previous = 0, "", None
        for x, c in enumerate(self.cells[y]):
            style = (c.style, c.selected, c.bold, c.background)
            if previous is not None and style != previous:
                yield start, value, previous
                start, value = x, ""
            value += c.char
            previous = style
        if previous:
            yield start, value, previous

    def plain(self) -> str:
        return "\n".join("".join(c.char for c in row).rstrip() for row in self.cells)

    def svg(self) -> str:
        # One text node per terminal cell preserves the actual layout even when
        # the fallback font has different glyph metrics.
        cw, ch, pad = 9, 20, 20
        parts = [f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.cols*cw+pad*2}" height="{self.rows*ch+pad*2}" viewBox="0 0 {self.cols*cw+pad*2} {self.rows*ch+pad*2}">',
                 f'<rect width="100%" height="100%" rx="16" fill="{PALETTE["bg"]}"/>',
                 '<g font-family="Menlo, DejaVu Sans Mono, monospace" font-size="15" xml:space="preserve">']
        for y, row in enumerate(self.cells):
            for x, c in enumerate(row):
                background = "selection" if c.selected else c.background
                if background:
                    parts.append(f'<rect x="{pad+x*cw}" y="{pad+y*ch}" width="{cw}" height="{ch}" fill="{PALETTE[background]}"/>')
                if c.char.strip():
                    parts.append(f'<text x="{pad+x*cw}" y="{pad+y*ch+15}" fill="{PALETTE.get(c.style, PALETTE["text"])}" font-weight="{"700" if c.bold else "400"}">{html.escape(c.char)}</text>')
        parts.extend(["</g>", "</svg>"])
        return "\n".join(parts)

    def save(self, path: Path):
        if path.suffix == ".svg":
            path.write_text(self.svg())
        elif path.suffix == ".html":
            path.write_text('<!doctype html><html lang="ru"><meta charset="utf-8"><title>ais · AI Sessions</title><body style="margin:0;background:#0b0e16;display:grid;place-items:center;min-height:100vh">' + self.svg() + "</body></html>")
        else:
            path.write_text(self.plain() + "\n")
        path.chmod(0o600)
