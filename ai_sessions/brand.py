"""The same compact wordmark appears in the terminal and exported previews."""

WORDMARK = (
    " ▄▀▀▄  ▀  ▄▀▀▀ ",
    " █▄▄█  █  ▀▀▀▄ ",
    " █  █  █  ▄▄▄▀ ",
)

# Hex colors also drive the terminal palette and SVG screenshot exporter.
PALETTE = {
    "bg": "#121212", "row": "#262626", "stripe": "#303030", "panel": "#1c1c1c", "selection": "#444444",
    "text": "#edf1f8", "muted": "#a0aec5", "faint": "#56657d",
    "accent": "#b9a3ff", "cyan": "#73d8f4", "green": "#95e1b4",
    "amber": "#f3ca86", "red": "#f291a6", "blue": "#95bbff",
}

CLIENT_STYLE = {"omp": "green", "claude": "amber", "codex": "blue", "agy": "accent", "gemini": "cyan", "pi": "red"}
STATUS_STYLE = {"ready": "green", "error": "red", "interrupted": "amber",
                "awaiting": "amber", "tool": "cyan", "thinking": "accent",
                "closed": "muted", "saved": "muted", "user": "blue"}
