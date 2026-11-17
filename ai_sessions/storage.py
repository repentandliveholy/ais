from __future__ import annotations

import json
import os
import sqlite3
import tempfile
from contextlib import contextmanager
from pathlib import Path

from .i18n import language
from .model import Session


@contextmanager
def read_database(path: Path):
    """Never create sidecars in a client's storage; respect a live WAL."""
    con = None
    try:
        if not path.exists():
            raise FileNotFoundError(path)
        # An immutable connection is safe only when no WAL is present.
        suffix = "?mode=ro" if Path(str(path) + "-wal").exists() else "?mode=ro&immutable=1"
        con = sqlite3.connect(path.resolve().as_uri() + suffix, uri=True, timeout=0.3)
        con.row_factory = sqlite3.Row
        con.execute("PRAGMA query_only=ON")
        yield con
    finally:
        if con:
            con.close()


def json_records(path: Path, full: bool = False):
    """Read complete JSONL records, tolerating partial concurrent writes.

    The overview samples head and tail so multi-gigabyte histories do not delay
    the picker. The focused detail scan streams the complete file separately.
    """
    with path.open("rb") as stream:
        size = os.fstat(stream.fileno()).st_size
        if full or size <= 384 * 1024:
            chunks = None
        else:
            head = stream.read(96 * 1024)
            stream.seek(max(len(head), size - 288 * 1024))
            stream.readline()  # discard the initial partial record
            tail = stream.read()
            chunks = [*head.splitlines()[:-1], *tail.splitlines()]
        for raw in stream if chunks is None else chunks:
            try:
                item = json.loads(raw)
                if isinstance(item, dict):
                    yield item
            except (ValueError, UnicodeError):
                continue


class Cache:
    VERSION = 5

    def __init__(self, root: Path | None, enabled: bool = True):
        self.root = root
        self.enabled = enabled and root is not None
        self.data: dict = {}
        if self.enabled:
            try:
                data = json.loads((root / "index.json").read_text())
                if data.get("version") == self.VERSION and isinstance(data.get("entries"), dict):
                    self.data = data.get("entries", {})
            except (OSError, ValueError, TypeError, AttributeError):
                pass

    @staticmethod
    def signature(path: Path) -> list[int]:
        st = path.stat()
        return [st.st_mtime_ns, st.st_size, st.st_ino]

    def get(self, path: Path, client: str, profile: str = "") -> Session | None:
        key = f"{language()}:{client}:{profile}:{path}"
        entry = self.data.get(key, {})
        try:
            if entry.get("sig") == self.signature(path):
                session = Session(**entry["session"])
                session._source_sig = entry["sig"]
                return session
        except (OSError, TypeError, KeyError, AttributeError):
            pass
        return None

    def put(self, path: Path, session: Session):
        try:
            signature = self.signature(path)
            if getattr(session, "_source_sig", signature) != signature:
                return  # A client appended a new turn while we were reading.
            self.data[f"{language()}:{session.client}:{session.profile}:{path}"] = {
                "sig": signature, "session": session.to_dict(),
            }
        except OSError:
            pass

    def save(self):
        if not self.enabled:
            return
        temp = None
        try:
            self.root.mkdir(mode=0o700, parents=True, exist_ok=True)
            # Only prune files that have disappeared; individual source failures
            # must not invalidate the remaining adapters' cache entries.
            entries = {k: v for k, v in self.data.items()
                       if Path(v["session"]["path"]).exists()}
            with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=self.root,
                                             prefix=".index-", delete=False) as f:
                temp = Path(f.name)
                os.chmod(temp, 0o600)
                json.dump({"version": self.VERSION, "entries": entries}, f, ensure_ascii=False)
            os.replace(temp, self.root / "index.json")
        except (OSError, KeyError, TypeError):
            pass
        finally:
            if temp:
                temp.unlink(missing_ok=True)


def default_cache(home: Path) -> Path:
    root = Path(os.environ.get("XDG_CACHE_HOME", str(home / ".cache")))
    return root / "ai-sessions"
