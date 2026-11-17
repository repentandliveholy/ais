"""Interface language and the small, persistent user preference."""
from __future__ import annotations

import json
import os
import tempfile
from pathlib import Path

from .locales import RU


LANGUAGES = ("en", "ru")
_language = "en"


def language() -> str:
    return _language


def set_language(value: str):
    global _language
    if value not in LANGUAGES:
        raise ValueError(f"Unsupported language: {value}")
    _language = value


def tr(message: str, **values) -> str:
    text = RU.get(message, message) if _language == "ru" else message
    return text.format(**values) if values else text


def settings_path() -> Path:
    root = Path(os.environ.get("XDG_CONFIG_HOME", str(Path.home() / ".config")))
    return root / "ai-sessions" / "settings.json"


def preferred_language() -> str:
    try:
        value = json.loads(settings_path().read_text()).get("language")
        return value if value in LANGUAGES else "en"
    except (OSError, ValueError, AttributeError, TypeError):
        return "en"


def read_settings() -> dict:
    try:
        settings = json.loads(settings_path().read_text())
        return settings if isinstance(settings, dict) else {}
    except (OSError, ValueError):
        return {}


def save_settings(**values):
    """Merge values into settings.json atomically, keeping unknown keys."""
    path = settings_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    settings = read_settings()
    settings.update(values)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".settings-", delete=False) as stream:
            temporary = Path(stream.name)
            os.chmod(temporary, 0o600)
            json.dump(settings, stream, ensure_ascii=False)
            stream.write("\n")
        os.replace(temporary, path)
    finally:
        if temporary:
            temporary.unlink(missing_ok=True)


def save_language(value: str):
    if value not in LANGUAGES:
        raise ValueError(f"Unsupported language: {value}")
    save_settings(language=value)
