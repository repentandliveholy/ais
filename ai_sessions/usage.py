from __future__ import annotations

import hashlib
import json
import math
import os
import re
import selectors
import shutil
import signal
import subprocess
import tempfile
import threading
import time
from concurrent.futures import CancelledError, ThreadPoolExecutor
from dataclasses import asdict, dataclass, field
from datetime import datetime, timedelta
from pathlib import Path
try:
    from zoneinfo import ZoneInfo, ZoneInfoNotFoundError
except ImportError:  # pragma: no cover - Python without tzdata
    ZoneInfo, ZoneInfoNotFoundError = None, Exception

from .claude_usage import read_claude_usage
from .i18n import tr
from .model import clean, line, stamp
from .screen import Screen, clip, width, wrap
from .screen import wrap as _wrap


PROVIDERS = {"anthropic": "Anthropic / Claude", "openai-codex": "OpenAI / Codex",
             "google-antigravity": "Google / agy", "deepseek": "DeepSeek API"}
CLI_SOURCES = {"anthropic": "Claude CLI", "openai-codex": "Codex CLI", "google-antigravity": "agy CLI",
               "deepseek": "DeepSeek API"}
DEEPSEEK_BALANCE_URL = "https://api.deepseek.com/user/balance"
CURRENCY = {"CNY": "¥", "USD": "$"}


def deepseek_key_path() -> Path:
    from .i18n import settings_path
    return settings_path().parent / "deepseek.key"


def deepseek_key() -> str:
    """DEEPSEEK_API_KEY, else the private key file written by --set-deepseek-key."""
    key = os.environ.get("DEEPSEEK_API_KEY", "").strip()
    if key:
        return key
    try:
        path = deepseek_key_path()
        if path.stat().st_size > 4096:
            return ""
        return path.read_text().strip()
    except OSError:
        return ""


def save_deepseek_key(key: str) -> Path:
    path = deepseek_key_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(descriptor, "w") as stream:
        stream.write(key.strip() + "\n")
    os.chmod(path, 0o600)
    return path


def money(amount: str, currency: str) -> str:
    try:
        value = f"{float(amount):,.2f}"
    except (TypeError, ValueError):
        value = line(amount, 20)
    symbol = CURRENCY.get(currency)
    return f"{symbol}{value}" if symbol else f"{value} {line(currency, 8)}"


def parse_deepseek_balance(payload: object, updated: float) -> list["Limit"]:
    """Documented /user/balance body: is_available + balance_infos[]."""
    if not isinstance(payload, dict) or not isinstance(payload.get("balance_infos"), list):
        raise ValueError(tr('Unsupported API response'))
    limits = []
    for info in payload["balance_infos"]:
        if not isinstance(info, dict):
            raise ValueError(tr('Unsupported API response'))
        currency = line(info.get("currency") or "", 8)
        total = money(info.get("total_balance"), currency)
        parts = [tr("topped up {amount}", amount=money(info.get("topped_up_balance"), currency)),
                 tr("granted {amount}", amount=money(info.get("granted_balance"), currency))]
        label = tr("balance {total}", total=total) + "  ·  " + "  ·  ".join(parts)
        try:
            empty = float(info.get("total_balance")) <= 0
        except (TypeError, ValueError):
            empty = False
        limits.append(Limit("deepseek", "API", label, currency or "—", None, 0, updated,
                            "DeepSeek API · live", status="balance-empty" if empty else "balance", id=currency))
    if not limits:
        raise ValueError(tr('API did not report a balance'))
    if payload.get("is_available") is False:
        limits[0].status = "balance-empty"
    return limits


def fetch_deepseek_balance(key: str, cancel, timeout: float) -> list["Limit"]:
    import urllib.error
    import urllib.request
    if cancel.is_set():
        raise CancelledError()
    request = urllib.request.Request(DEEPSEEK_BALANCE_URL, headers={
        "Authorization": "Bearer " + key, "Accept": "application/json", "User-Agent": "ais"})
    try:
        with urllib.request.urlopen(request, timeout=min(timeout, 15)) as response:
            body = response.read(256 * 1024)
    except urllib.error.HTTPError as exc:
        # Never echo the response body or the key; status codes are enough.
        if exc.code in (401, 403):
            raise ValueError(tr('DeepSeek rejected the API key (HTTP {code})', code=exc.code)) from None
        raise ValueError(tr('DeepSeek API error (HTTP {code})', code=exc.code)) from None
    except (urllib.error.URLError, TimeoutError, OSError):
        raise ValueError(tr('DeepSeek API is unreachable')) from None
    if cancel.is_set():
        raise CancelledError()
    try:
        payload = json.loads(body)
    except ValueError:
        raise ValueError(tr('Unsupported API response')) from None
    return parse_deepseek_balance(payload, time.time())


@dataclass
class Limit:
    provider: str
    account: str
    label: str
    window: str
    used: float | None
    reset: float
    updated: float
    source: str
    status: str = ""
    id: str = ""
    # Preserve native human reset text when the CLI does not export an epoch.
    reset_label: str = ""

    def to_dict(self):
        return asdict(self)


@dataclass
class Report:
    provider: str
    text: str
    updated: float
    cached: bool = False

    def to_dict(self):
        return asdict(self)


@dataclass
class Account:
    """Who a set of limits belongs to, as reported by the official CLI."""
    provider: str
    key: str
    email: str = ""
    name: str = ""
    plan: str = ""
    dirs: list[str] = field(default_factory=list)
    expires: float = 0
    sessions_5h: int = 0
    sessions_7d: int = 0
    credits: str = ""
    warning: str = ""

    def to_dict(self):
        return asdict(self)

    @property
    def title(self) -> str:
        return self.email or self.name or self.key


@dataclass
class Usage:
    limits: list[Limit] = field(default_factory=list)
    errors: list[str] = field(default_factory=list)
    loaded: bool = False
    live: bool = False
    reports: list[Report] = field(default_factory=list)
    # Last attempt, separately from the successful snapshot timestamps.
    checked: float = 0
    accounts: list[Account] = field(default_factory=list)

    def account(self, provider: str, key: str) -> Account | None:
        return next((a for a in self.accounts if a.provider == provider and a.key == key), None)


WINDOW_SECONDS = {"5h": 5 * 3600, "week": 7 * 86400}
PLAN_NAMES = {"team": "Team", "max": "Max", "pro": "Pro", "plus": "Plus", "free": "Free",
              "enterprise": "Enterprise", "business": "Business", "edu": "Edu"}


def plan_name(value) -> str:
    value = line(value or "", 40)
    return PLAN_NAMES.get(value.casefold(), value[:1].upper() + value[1:])


def local_activity(folder: Path, now: float | None = None, suffix: str = ".jsonl") -> tuple[int, int]:
    """Session files touched in the last 5 hours / 7 days (mtime only)."""
    now = now or time.time()
    recent = week = 0
    if not folder.is_dir():
        return 0, 0
    for root, dirs, files in os.walk(folder):
        dirs[:] = [d for d in dirs if d != "subagents"]
        for name in files:
            if not name.endswith(suffix) or name.startswith("agent-"):
                continue
            try:
                age = now - os.stat(os.path.join(root, name)).st_mtime
            except OSError:
                continue
            if age <= 7 * 86400:
                week += 1
                if age <= 5 * 3600:
                    recent += 1
    return recent, week


_RESET = re.compile(r"Resets\s+(?:(?P<month>[A-Z][a-z]{2})\s+(?P<day>\d{1,2})(?:\s+at)?\s+)?"
                    r"(?P<hour>\d{1,2})(?::(?P<minute>\d{2}))?\s*(?P<ampm>am|pm)"
                    r"(?:\s*\((?P<zone>[A-Za-z_]+(?:/[A-Za-z0-9_+-]+)*)\))?\s*$", re.I)
_MONTHS = ("jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec")


def parse_reset_text(text: str, now: float | None = None) -> float:
    """'Resets 5:20pm (Europe/Berlin)' or 'Resets Nov 15 at 6am (Zone)' -> epoch.

    Only these exact native forms are converted; anything else stays a label.
    """
    match = _RESET.search(text or "")
    if not match:
        return 0
    now = now or time.time()
    zone = None
    if match["zone"] and ZoneInfo is not None:
        try:
            zone = ZoneInfo(match["zone"])
        except (ZoneInfoNotFoundError, ValueError):
            return 0
    current = datetime.fromtimestamp(now, zone) if zone else datetime.fromtimestamp(now)
    hour = int(match["hour"]) % 12 + (12 if match["ampm"].lower() == "pm" else 0)
    minute = int(match["minute"] or 0)
    if minute > 59 or int(match["hour"]) > 12:
        return 0
    try:
        if match["month"]:
            month = _MONTHS.index(match["month"].lower()) + 1
            target = current.replace(month=month, day=int(match["day"]), hour=hour, minute=minute, second=0, microsecond=0)
            if target.timestamp() < now - 86400:
                target = target.replace(year=target.year + 1)
        else:
            target = current.replace(hour=hour, minute=minute, second=0, microsecond=0)
            if target.timestamp() <= now - 60:
                target += timedelta(days=1)
    except ValueError:
        return 0
    return target.timestamp()


def pace(limit: Limit, now: float) -> tuple[str, str] | None:
    """Project the current burn rate to the reset; None when unknowable."""
    span = WINDOW_SECONDS.get(limit.window)
    if not span or limit.used is None or not limit.reset or limit.reset <= now:
        return None
    left = limit.reset - now
    elapsed = span - left
    if elapsed < span * .03 or limit.used <= 0:
        return None
    early = elapsed < span * .15
    rate = limit.used / elapsed
    expected = elapsed / span
    if limit.used >= 1:
        return tr("limit reached · wait for reset"), "red"
    runs_out = (1 - limit.used) / rate
    if runs_out < left * .92 and not early:
        return tr("at this pace runs out in {time} — before reset", time=duration_text(runs_out)), "red" if runs_out < left * .5 else "amber"
    if limit.used > expected + .1:
        return tr("ahead of an even pace ({used:.0f}% used, {expected:.0f}% of window elapsed)",
                  used=limit.used * 100, expected=expected * 100), "amber"
    return tr("on pace · {expected:.0f}% of window elapsed", expected=expected * 100), "green"


def duration_text(seconds: float) -> str:
    seconds = max(60, int(seconds))
    if seconds >= 86400:
        return tr("{d}d {h}h", d=seconds // 86400, h=seconds % 86400 // 3600)
    if seconds >= 3600:
        return tr("{h}h {m}m", h=seconds // 3600, m=seconds % 3600 // 60)
    return tr("{m}m", m=seconds // 60)


def numeric(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def timestamp(value) -> float:
    return float(value) if numeric(value) and 0 < value < 253402300800 else 0


def window_name(value: str, label: str = "") -> str:
    text = (value + " " + label).lower()
    if any(s in text for s in ("5 hour", "5-hour", "5h", "300 min")):
        return "5h"
    if any(s in text for s in ("week", "7 day", "7-day", "1w", "10080")):
        return "week"
    if "month" in text:
        return "month"
    return line(value or label or tr('Limit'), 30)


def anonymous_account(value: str) -> str:
    return "#" + hashlib.sha256(value.encode()).hexdigest()[:6]


def parse_codex_usage(payload: object, updated: float) -> list[Limit]:
    """Documented account/rateLimits/read result, never credentials or rollouts.

    https://developers.openai.com/codex/app-server/#rate-limits-chatgpt
    The public multi-bucket view supersedes the duplicate legacy bucket.
    """
    if not isinstance(payload, dict):
        raise ValueError(tr('Unsupported CLI response'))
    buckets = payload.get("rateLimitsByLimitId")
    if buckets is not None and not isinstance(buckets, dict):
        raise ValueError(tr('Unsupported CLI response'))
    if not buckets:
        single = payload.get("rateLimits")
        if not isinstance(single, dict):
            raise ValueError(tr('CLI did not report limits'))
        buckets = {"codex": single}
    account = anonymous_account(payload["accountId"]) if isinstance(payload.get("accountId"), str) and payload["accountId"] else "CLI"
    limits = []
    for key, bucket in buckets.items():
        if not isinstance(bucket, dict):
            raise ValueError(tr('Unsupported CLI response'))
        for kind in ("primary", "secondary"):
            window = bucket.get(kind)
            if window is None:
                continue
            if not isinstance(window, dict):
                raise ValueError(tr('Unsupported CLI response'))
            minutes = window.get("windowDurationMins")
            name = "5h" if minutes == 300 else "week" if minutes == 10080 else f"{minutes:g}m" if numeric(minutes) and minutes > 0 else kind
            used = window.get("usedPercent")
            # Reject invalid percentages rather than displaying invented values.
            fraction = float(used) / 100 if numeric(used) and 0 <= used <= 100 else None
            limits.append(Limit("openai-codex", account, line(bucket.get("limitName") or bucket.get("limitId") or key, 80),
                                name, fraction, timestamp(window.get("resetsAt")), updated,
                                "Codex CLI · live", id=line(key, 80) + ":" + kind))
    if not limits:
        raise ValueError(tr('CLI did not report limits'))
    return limits


def _cli_output(command, env, cwd, cancel, timeout, *, rpc=False):
    """Bounded, cancellable official CLI transport; stderr never enters the UI.

    Only initialize and rate-limit read are allowed. No thread or model turn is
    created. Server requests (including external auth refresh) are declined.
    """
    if cancel.is_set():
        raise CancelledError()
    with subprocess.Popen(command, stdin=subprocess.PIPE if rpc else subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, env=env,
                          cwd=cwd, start_new_session=True) as process:
        try:
            def send(message):
                process.stdin.write((json.dumps(message) + "\n").encode())
                process.stdin.flush()

            if rpc:
                send({"id": 1, "method": "initialize", "params": {
                    "clientInfo": {"name": "ai_sessions", "title": "ais Usage", "version": "1.0"}}})
            deadline = time.monotonic() + timeout
            buffer, output, total, expected = b"", [], 0, 1
            # account/read is optional identity metadata: a short grace period,
            # and any failure keeps the already received rate limits.
            results, account_deadline = {}, float("inf")
            with selectors.DefaultSelector() as selector:
                selector.register(process.stdout, selectors.EVENT_READ)
                while True:
                    if cancel.is_set():
                        raise CancelledError()
                    if expected == 3 and time.monotonic() >= account_deadline:
                        return results
                    if time.monotonic() >= deadline:
                        raise TimeoutError(tr('CLI request timed out'))
                    if not selector.select(min(.1, max(0, deadline - time.monotonic()))):
                        continue
                    chunk = os.read(process.stdout.fileno(), 65536)
                    if not chunk and expected == 3:
                        return results
                    if not chunk:
                        while process.poll() is None:
                            if cancel.is_set():
                                raise CancelledError()
                            if time.monotonic() >= deadline:
                                raise TimeoutError(tr('CLI request timed out'))
                            cancel.wait(.05)
                        code = process.returncode
                        if code:
                            raise ValueError(tr('CLI exited with code {code}', code=code))
                        if rpc:
                            raise ValueError(tr('CLI closed before replying'))
                        return b"".join(output).decode("utf-8", errors="replace")
                    total += len(chunk)
                    if total > 2 * 1024 * 1024:
                        raise ValueError(tr('CLI response too large'))
                    if not rpc:
                        output.append(chunk)
                        continue
                    buffer += chunk
                    while b"\n" in buffer:
                        raw, buffer = buffer.split(b"\n", 1)
                        try:
                            message = json.loads(raw)
                        except (ValueError, UnicodeError):
                            raise ValueError(tr('Unsupported CLI response')) from None
                        if not isinstance(message, dict):
                            raise ValueError(tr('Unsupported CLI response'))
                        if "method" in message:
                            if "id" in message:
                                send({"id": message["id"], "error": {"code": -32601, "message": "Unsupported client request"}})
                            continue
                        if expected == 2 and message.get("id") == 3:
                            if isinstance(message.get("result"), dict):
                                results["account"] = message["result"]
                            continue
                        if message.get("id") != expected:
                            continue
                        if "error" in message and expected == 3:
                            return results
                        if "error" in message:
                            error = message["error"]
                            code = error.get("code") if isinstance(error, dict) else None
                            if not isinstance(code, int) or isinstance(code, bool):
                                code = "?"
                            raise ValueError(tr('CLI request rejected (code {code}); check login in the official CLI', code=code))
                        if "result" not in message:
                            raise ValueError(tr('Unsupported CLI response'))
                        if expected == 1:
                            send({"method": "initialized", "params": {}})
                            send({"id": 2, "method": "account/rateLimits/read"})
                            send({"id": 3, "method": "account/read", "params": {}})
                            expected = 2
                        elif expected == 2:
                            results["limits"] = message["result"]
                            if "account" in results:
                                return results
                            expected, account_deadline = 3, time.monotonic() + .4
                        else:
                            results["account"] = message["result"]
                            return results
        except (BrokenPipeError, subprocess.TimeoutExpired):
            raise ValueError(tr('CLI closed before replying')) from None
        finally:
            # Include any children the official CLI started; do not leave a
            # provider request or language server running after Q / timeout.
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()


def unavailable(provider: str, reason: str) -> Limit:
    return Limit(provider, "CLI", reason, "—", None, 0, 0, "Official CLI", "unavailable")


def _claude_status() -> Limit:
    return unavailable("anthropic", 'No saved Claude CLI limits' if shutil.which("claude") else 'Claude CLI not installed')


def parse_claude_usage(windows: list[dict], updated: float) -> list[Limit]:
    limits = []
    for window in windows:
        heading = window["label"]
        if heading.casefold() == "current session":
            name, label = "5h", "Claude"
        else:
            name = "week"
            model = re.search(r"\(([^)]+)\)", heading)
            label = 'All Claude models' if not model or model[1].casefold() == 'all models' else model[1]
        limits.append(Limit("anthropic", "CLI", label, name, window["used_percent"] / 100,
                            0, updated, "Claude CLI · live", id=heading,
                            reset_label=window["reset_text"]))
    if not limits:
        raise ValueError(tr('Claude /usage did not provide plan limits'))
    return limits


def _cache_path(collector):
    if not collector.cache.enabled:
        return None
    # Isolate alternate homes and --source stores without saving their paths.
    context = str(collector.home.resolve()) + repr({client: [str(p.resolve()) for p in collector.root(client)] for client in ("codex", "agy", "claude")})
    key = hashlib.sha256(context.encode()).hexdigest()[:16]
    return collector.cache.root / f"official-usage-{key}.json"


def load_usage(collector) -> Usage:
    """Only ais snapshots produced by the official adapters; no OMP database."""
    usage = Usage(loaded=True)
    path = _cache_path(collector)
    if path and path.exists():
        try:
            if path.stat().st_size > 2 * 1024 * 1024:
                raise ValueError()
            payload = json.loads(path.read_text())
            if payload.get("version") != 1:
                raise ValueError()
            saved_limits = []
            for item in payload["limits"]:
                r = Limit(**item)
                if (r.provider not in CLI_SOURCES or r.source != CLI_SOURCES[r.provider] + " · live" or
                        not timestamp(r.updated) or (r.used is not None and (not numeric(r.used) or not 0 <= r.used <= 1)) or
                        not numeric(r.reset) or r.reset < 0 or r.reset >= 253402300800 or
                        not all(isinstance(v, str) for v in (r.account, r.label, r.window, r.id, r.reset_label))):
                    raise ValueError()
                r.source = CLI_SOURCES[r.provider] + " · cached"
                saved_limits.append(r)
            # Migrate earlier official text snapshots into the same normalized
            # bars; unsupported old text never substitutes for current quotas.
            for item in payload["reports"]:
                r = Report(**item)
                if r.provider != "google-antigravity" or not isinstance(r.text, str) or not timestamp(r.updated):
                    raise ValueError()
                try:
                    migrated = parse_agy_usage(r.text, r.updated)
                    for limit in migrated:
                        limit.source = "agy CLI · cached"
                    saved_limits += migrated
                except ValueError:
                    usage.errors.append(tr('Cached agy report format unavailable; refresh with the official CLI'))
            usage.limits += saved_limits
            for item in payload.get("accounts") or []:
                try:
                    account = Account(**item)
                except TypeError:
                    continue
                if isinstance(account.key, str) and isinstance(account.dirs, list):
                    usage.accounts.append(account)
        except (OSError, ValueError, TypeError, KeyError, AttributeError):
            usage.errors.append(tr('Official CLI cache could not be read'))
    if not any(r.provider == "anthropic" for r in usage.limits):
        usage.limits.insert(0, _claude_status())
    if not any(r.provider == "openai-codex" for r in usage.limits):
        usage.limits.append(unavailable("openai-codex", 'No saved Codex CLI limits'))
    if not any(r.provider == "google-antigravity" for r in usage.limits):
        usage.limits.append(unavailable("google-antigravity", 'No saved agy CLI limits'))
    return usage


def _save_usage(collector, usage):
    path = _cache_path(collector)
    if path is None:
        return
    temporary = None
    try:
        path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        # Failed providers retain their last successful snapshot and timestamp.
        limits = [dict(r.to_dict(), source=CLI_SOURCES[r.provider] + " · live")
                  for r in usage.limits if r.provider in CLI_SOURCES and r.status != "unavailable"]
        reports = [dict(r.to_dict(), cached=False) for r in usage.reports]
        with tempfile.NamedTemporaryFile(mode="w", encoding="utf-8", dir=path.parent,
                                         prefix=".official-usage-", delete=False) as stream:
            temporary = stream.name
            os.chmod(temporary, 0o600)
            json.dump({"version": 1, "limits": limits, "reports": reports,
                       "accounts": [a.to_dict() for a in usage.accounts]}, stream, ensure_ascii=False)
        os.replace(temporary, path)
    except OSError:
        usage.errors.append(tr('Official CLI cache could not be saved'))
    finally:
        if temporary:
            try:
                os.unlink(temporary)
            except FileNotFoundError:
                pass


def _agy_report(text: str) -> str:
    # Sanitize the public report and preserve the existing model-group filter.
    # The TSV parser below validates its columns before normalizing quotas.
    text = clean(text).strip()
    text = re.sub(r"[\w.+-]+@[\w.-]+\.[A-Za-z]{2,}", "[account]", text)
    if re.search(r"(?im)^\s*(error|failed|unauthorized|forbidden)\b", text):
        raise ValueError(tr('CLI could not report limits; check the official CLI'))
    # Preserve the existing Gemini-only display preference. Once a Claude/GPT
    # group starts, omit its continuation rows until the next Gemini heading.
    rows, hidden = [], False
    for row in text.splitlines():
        if re.search(r"claude|gpt", row, re.I):
            hidden = True
        elif re.search(r"gemini", row, re.I):
            hidden = False
        if not hidden:
            rows.append(row)
    text = "\n".join(rows).strip()
    if not text or not re.search(r"%|remaining|reset|unlimited", text, re.I):
        raise ValueError(tr('CLI did not report limits'))
    return text


def parse_agy_usage(text: str, updated: float) -> list[Limit]:
    """Official `agy -p /usage` TSV: group, quota, remaining %, reset ISO.

    The percentage column reports remaining quota, including model-specific
    Weekly Limit rows. No token counters or guessed quota capacities are used.
    """
    text = _agy_report(text)
    limits = []
    seen = set()
    for row in text.splitlines():
        if not row.strip():
            continue
        columns = row.split("\t")
        if len(columns) != 4:
            raise ValueError(tr('Unsupported agy usage report; expected official quota rows'))
        group, label, amount, reset = (value.strip() for value in columns)
        percent = re.fullmatch(r"(\d+(?:\.\d+)?)%", amount)
        if not group or not label or not percent or not 0 <= float(percent[1]) <= 100:
            raise ValueError(tr('Unsupported agy usage report; expected official quota rows'))
        if "weekly" in label.casefold():
            window = "week"
        elif re.search(r"five[ -]hour|5[ -]hour", label, re.I):
            window = "5h"
        else:
            raise ValueError(tr('Unsupported agy quota window'))
        reset_at = timestamp(stamp(reset)) if re.search(r"(?:Z|[+-]\d\d:\d\d)$", reset) else 0
        if not reset_at:
            raise ValueError(tr('Unsupported agy quota reset time'))
        key = (group, label)
        if key in seen:
            continue
        seen.add(key)
        limits.append(Limit("google-antigravity", "CLI", line(group, 80), window,
                            1 - float(percent[1]) / 100, reset_at, updated,
                            "agy CLI · live", id=group + ":" + label))
    if not limits:
        raise ValueError(tr('CLI did not report limits'))
    limits.sort(key=lambda r: 0 if r.window == "5h" else 1)
    return limits


def claude_stores(collector) -> list[tuple[Path, bool]]:
    """(config dir, is the CLI default) for every Claude store ais reads."""
    default = collector.home / ".claude"
    plain = collector.home == Path.home() and not os.environ.get("CLAUDE_CONFIG_DIR") and "claude" not in collector.overrides
    return [(root, plain and root == default) for root in collector.root("claude")]


def claude_identity(binary, env, cwd, cancel, timeout) -> dict:
    """`claude auth status --json`: documented, local, no model turn."""
    try:
        data = json.loads(_cli_output([binary, "auth", "status", "--json"], env, cwd, cancel, min(timeout, 6)))
    except (ValueError, TimeoutError, OSError):
        return {}
    return data if isinstance(data, dict) else {}


def trusted_dir(config: Path, fallback: Path) -> Path:
    """A folder this Claude config already trusts, so /usage never needs a prompt."""
    try:
        if config.stat().st_size > 64 * 1024 * 1024:
            return fallback
        projects = json.loads(config.read_text()).get("projects") or {}
    except (OSError, ValueError, AttributeError):
        return fallback
    if isinstance(projects.get(str(fallback)), dict) and projects[str(fallback)].get("hasTrustDialogAccepted"):
        return fallback
    for path, info in projects.items():
        if isinstance(info, dict) and info.get("hasTrustDialogAccepted") and Path(path).is_dir():
            return Path(path)
    return fallback


_AGY_AUTH = re.compile(r"applyAuthResult: email=([^,\s]+), authMethod=(\w+)")
AGY_METHODS = {"consumer": "Google account", "enterprise": "Workspace", "vertex": "Vertex AI"}


def agy_identity(store: Path, started: float) -> tuple[str, str]:
    """(email, auth method) from the agy log written by this /usage run.

    agy has no account command; its own log records the signed-in OAuth user.
    Only logs created by the run are read, so an old login is never reused.
    """
    try:
        logs = [p for p in (store / "log").glob("cli-*.log") if p.stat().st_mtime >= started - 2]
    except OSError:
        return "", ""
    for path in sorted(logs, key=lambda p: p.stat().st_mtime, reverse=True):
        try:
            with path.open("rb") as stream:
                text = stream.read(2 * 1024 * 1024).decode("utf-8", errors="replace")
        except OSError:
            continue
        found = _AGY_AUTH.findall(text)
        if found:
            email, method = found[-1]
            return line(email, 120), method
    return "", ""


def account_key(*parts: str) -> str:
    return anonymous_account("|".join(p for p in parts if p)) if any(parts) else "CLI"


def pretty_dir(path: Path, home: Path) -> str:
    text = str(path)
    return "~" + text[len(str(home)):] if text.startswith(str(home) + "/") else text


def fetch_live_usage(collector, cancel: threading.Event | None = None, timeout: float = 30) -> Usage:
    cancel = cancel or threading.Event()
    if cancel.is_set():
        raise CancelledError()
    usage = load_usage(collector)
    usage.checked = time.time()

    def base_env():
        env = os.environ.copy()
        env["HOME"] = str(collector.home)
        return env

    def fetch_codex(binary):
        roots = collector.root("codex")
        if not roots:
            raise ValueError(tr('CLI source not configured'))
        env = base_env()
        env["CODEX_HOME"] = str(roots[0])
        result = _cli_output([binary, "app-server"], env, collector.home, cancel, timeout, rpc=True)
        limits = parse_codex_usage(result.get("limits"), time.time())
        info = result.get("account") if isinstance(result.get("account"), dict) else {}
        account = info.get("account") if isinstance(info.get("account"), dict) else {}
        raw = result.get("limits") if isinstance(result.get("limits"), dict) else {}
        bucket = raw.get("rateLimits") if isinstance(raw.get("rateLimits"), dict) else {}
        key = limits[0].account if limits else "CLI"
        details = Account("openai-codex", key, email=line(account.get("email") or "", 120),
                          name=line(account.get("workspaceName") or "", 80),
                          plan=plan_name(account.get("planType") or bucket.get("planType") or ""),
                          dirs=[pretty_dir(roots[0], collector.home)])
        credits = bucket.get("credits") if isinstance(bucket.get("credits"), dict) else {}
        if credits.get("unlimited"):
            details.credits = tr("unlimited credits")
        elif credits.get("hasCredits"):
            details.credits = tr("credits: {balance}", balance=line(credits.get("balance"), 20))
        if bucket.get("rateLimitReachedType") or bucket.get("spendControlReached"):
            details.warning = tr("limit reached: {kind}", kind=line(bucket.get("rateLimitReachedType") or "spend control", 40))
        details.sessions_5h, details.sessions_7d = local_activity(roots[0] / "sessions")
        return limits, [details]

    def fetch_claude(binary):
        stores = claude_stores(collector)
        if not stores:
            raise ValueError(tr('CLI source not configured'))
        identities: dict[str, tuple[dict, list[tuple[Path, bool]]]] = {}
        with ThreadPoolExecutor(max_workers=4) as pool:
            def identify(store):
                env = base_env()
                if store[1]:
                    env.pop("CLAUDE_CONFIG_DIR", None)
                else:
                    env["CLAUDE_CONFIG_DIR"] = str(store[0])
                return claude_identity(binary, env, collector.home, cancel, timeout), env
            found = list(pool.map(identify, stores))
        envs = {}
        for store, (status, env) in zip(stores, found):
            if status and status.get("loggedIn") is False:
                key = account_key("logged-out", str(store[0]))
            else:
                key = account_key(status.get("email", ""), status.get("orgId", "")) if status else account_key(str(store[0]))
            identities.setdefault(key, (status, []))[1].append(store)
            config = collector.home / ".claude.json" if store[1] else store[0] / ".claude.json"
            envs.setdefault(key, (env, trusted_dir(config, Path.cwd())))
        accounts, limits, errors = [], [], []

        def per_account(key):
            status, members = identities[key]
            email, org = line(status.get("email") or "", 120), line(status.get("orgName") or "", 80)
            if email and org.casefold() in (f"{email}'s organization".casefold(), email.casefold()):
                org = ""  # A personal default organization only repeats the email.
            details = Account("anthropic", key, email=email, name=org,
                              plan=plan_name(status.get("subscriptionType") or ""),
                              dirs=[pretty_dir(root, collector.home) + ("" if not default else tr(" (default)"))
                                    for root, default in members],
                              expires=stamp(status.get("loginExpiresAt") or 0))
            for root, _ in members:
                recent, week = local_activity(root / "projects")
                details.sessions_5h += recent
                details.sessions_7d += week
            if status.get("loggedIn") is False:
                return details, [], tr("not logged in · run claude /login with this config dir")
            try:
                env, cwd = envs[key]
                windows = read_claude_usage(binary, env, cwd, cancel, timeout)
                rows = parse_claude_usage(windows, time.time())
            except (OSError, ValueError, TimeoutError) as exc:
                return details, [], str(exc) if not isinstance(exc, OSError) or isinstance(exc, TimeoutError) else tr('Could not launch official CLI')
            for row in rows:
                row.account = key
                row.reset = parse_reset_text(row.reset_label)
            return details, rows, ""

        with ThreadPoolExecutor(max_workers=3) as pool:
            for details, rows, error in pool.map(per_account, list(identities)):
                accounts.append(details)
                limits += rows
                if error:
                    errors.append(error)
                    limits.append(Limit("anthropic", details.key, error, "—", None, 0, 0, "Official CLI", "unavailable"))
        if errors and not any(r.status != "unavailable" for r in limits):
            raise ValueError(errors[0])
        return limits, accounts

    def fetch(client):
        if client == "deepseek":
            key = deepseek_key()
            limits = fetch_deepseek_balance(key, cancel, timeout)
            for limit in limits:
                limit.account = "key …" + key[-4:]
            return limits, []
        binary = shutil.which(client)
        if not binary:
            raise ValueError(tr('Official CLI not installed'))
        if client == "codex":
            return fetch_codex(binary)
        if client == "claude":
            return fetch_claude(binary)
        # agy has no documented flag/environment variable for alternate stores.
        if collector.root("agy") != [collector.home / ".gemini/antigravity-cli"]:
            raise ValueError(tr('agy CLI does not support an alternate usage store'))
        store = collector.home / ".gemini/antigravity-cli"
        started = time.time()
        result = _cli_output([binary, "-p", "/usage"], base_env(), collector.home, cancel, timeout)
        limits = parse_agy_usage(result, time.time())
        email, method = agy_identity(store, started)
        key = account_key(email) if email else "CLI"
        for limit in limits:
            limit.account = key
        details = Account("google-antigravity", key, email=email,
                          plan=AGY_METHODS.get(method, method), dirs=[pretty_dir(store, collector.home)])
        details.sessions_5h, details.sessions_7d = local_activity(store / "conversations", suffix=".db")
        return limits, [details]

    with ThreadPoolExecutor(max_workers=4) as pool:
        clients = ("claude", "codex", "agy", *(("deepseek",) if deepseek_key() else ()))
        pending = {client: pool.submit(fetch, client) for client in clients}
        for client, future in pending.items():
            provider = {"claude": "anthropic", "codex": "openai-codex", "agy": "google-antigravity",
                        "deepseek": "deepseek"}[client]
            try:
                value, accounts = future.result()
                usage.limits = [r for r in usage.limits if r.provider != provider]
                usage.limits += value
                usage.accounts = [a for a in usage.accounts if a.provider != provider] + accounts
                usage.live = True
            except CancelledError:
                raise
            except (OSError, ValueError, TimeoutError) as exc:
                # OS exception text can contain paths; provider stderr and RPC
                # error messages can contain identities. Show only safe reasons.
                reason = tr('Could not launch official CLI') if isinstance(exc, OSError) and not isinstance(exc, TimeoutError) else str(exc)
                usage.errors.append(client + ": " + reason)
                if not any(r.provider == provider and r.status != "unavailable" for r in usage.limits):
                    usage.limits = [r for r in usage.limits if r.provider != provider]
                    usage.limits.append(unavailable(provider, 'Limits unavailable; see source errors'))
    if cancel.is_set():
        raise CancelledError()
    _save_usage(collector, usage)
    return usage


def demo_usage() -> Usage:
    now = time.time()
    usage = Usage([
        Limit("anthropic", "#demo01", "Claude", "5h", .38, now + 8200, now - 180, "Demo"),
        Limit("anthropic", "#demo01", tr('All Claude models'), "week", .64, now + 3 * 86400, now - 180, "Demo"),
        Limit("openai-codex", "#demo02", "Codex", "5h", .91, now + 2400, now - 60, "Demo"),
        Limit("openai-codex", "#demo02", "Codex", "week", .73, now + 4 * 86400, now - 60, "Demo"),
        Limit("google-antigravity", "#demo03", "Gemini", "5h", .12, now + 13000, now - 240, "Demo"),
        Limit("google-antigravity", "#demo03", "Gemini", "week", .85, now + 2 * 86400, now - 240, "Demo"),
    ], loaded=True)
    usage.limits[2:2] = [
        Limit("anthropic", "#demo04", "Claude", "5h", .72, now + 3 * 3600, now - 200, "Demo"),
        Limit("anthropic", "#demo04", tr('All Claude models'), "week", .31, now + 5 * 86400, now - 200, "Demo"),
    ]
    usage.accounts = [
        Account("anthropic", "#demo01", "dev@example.com", "Example Team", "Team", ["~/.claude (default)"], now + 20 * 86400, 3, 41),
        Account("anthropic", "#demo04", "side@example.com", "", "Max", ["~/.claude-side"], now + 9 * 86400, 1, 12),
        Account("openai-codex", "#demo02", "dev@example.com", "", "Plus", ["~/.codex"], 0, 2, 17),
        Account("google-antigravity", "#demo03", "", "", "", [], 0, 0, 0),
    ]
    return usage


def reset_text(value: float, now: float) -> str:
    if not value:
        return tr('reset not recorded')
    seconds = int(value - now)
    if seconds <= 0:
        return tr('window expired · refresh')
    if seconds >= 86400:
        return tr('resets in {v0}d {v1}h', v0=seconds // 86400, v1=seconds % 86400 // 3600)
    if seconds >= 3600:
        return tr('resets in {v0}h {v1}m', v0=seconds // 3600, v1=seconds % 3600 // 60)
    return tr('resets in {v0}m', v0=max(1, seconds // 60))


def snapshot_text(updated: float) -> str:
    if not updated:
        return tr('Snapshot time unknown')
    return tr('Snapshot: {timestamp} UTC', timestamp=time.strftime("%Y-%m-%d %H:%M:%S", time.gmtime(updated)))


def usage_content(usage: Usage, cols: int, now: float) -> list[tuple[str, str]]:
    content: list[tuple[str, str]] = []

    def wrap(text: str, maximum: int) -> list[str]:
        indent = len(text) - len(text.lstrip(" "))
        return [" " * indent + row for row in _wrap(text.strip(), maximum - indent)]

    previous = None
    providers = [r.provider for r in usage.limits]
    multiple = {p for p in providers if len({r.account for r in usage.limits if r.provider == p and r.account != "CLI"}) > 1}
    for r in usage.limits:
        group = (r.provider, r.account)
        if group != previous:
            if previous:
                content.append(("", "text"))
            account = usage.account(r.provider, r.account)
            who = account.title if account else r.account
            heading = PROVIDERS.get(r.provider, r.provider) + (f"  ·  {who}" if who != "CLI" else "")
            if account:
                heading += "".join(f"  ·  {v}" for v in (account.name, account.plan) if v)
            source = r.source if r.status != "unavailable" else ""
            content.extend((text, "cyan") for text in wrap(heading + (f"  ·  {tr(source)}" if source else ""), cols - 4))
            if r.provider in multiple:
                content.append(("  " + tr("separate account · limits are not shared with other {provider} accounts",
                                   provider=PROVIDERS.get(r.provider, r.provider).split(" / ")[-1]), "accent"))
            if account:
                facts = []
                if account.dirs:
                    facts.append(tr("config: {dirs}", dirs=", ".join(account.dirs)))
                if account.expires:
                    facts.append(tr("login valid until {date}", date=time.strftime("%Y-%m-%d", time.localtime(account.expires))))
                if account.credits:
                    facts.append(account.credits)
                if facts:
                    content.extend((text, "muted") for text in wrap("  " + "  ·  ".join(facts), cols - 4))
                if account.dirs:
                    content.extend((text, "muted") for text in wrap("  " + tr("local sessions — last 5h: {recent} · last 7 days: {week}",
                                                                               recent=account.sessions_5h, week=account.sessions_7d), cols - 4))
                if len(account.dirs) > 1:
                    content.extend((text, "muted") for text in wrap("  " + tr("these config dirs are signed in to the same account and share its limits"), cols - 4))
                if account.warning:
                    content.append(("  ⚠ " + account.warning, "red"))
            if r.updated:
                content.extend((text, "muted") for text in wrap("  " + snapshot_text(r.updated), cols - 4))
            previous = group
        if r.status == "unavailable":
            content.extend((text, "amber") for text in wrap(tr(r.label), cols - 4))
            continue
        if r.status in ("balance", "balance-empty"):
            empty = r.status == "balance-empty"
            content.extend((text, "red" if empty else "green") for text in wrap(f"  {r.window:6} " + r.label, cols - 4))
            if empty:
                content.append(("  ⚠ " + tr("balance exhausted · top up to keep using the API"), "red"))
            continue
        expired = bool(r.reset and r.reset <= now)
        stale = now - r.updated > 3600 or expired
        style = "muted" if stale else "red" if r.used is not None and r.used >= .9 else "amber" if r.used is not None and r.used >= .7 else "green"
        bar_width = 8 if cols < 70 else 16
        if r.used is None:
            meter = "░" * bar_width + tr('  remaining usage unknown')
        else:
            filled = round(r.used * bar_width)
            percentages = (tr('  {v0:3.0f}% used · {v1:3.0f}% remaining', v0=r.used * 100, v1=(1 - r.used) * 100)
                           if cols >= 70 else tr("  {used:.0f}% used · {remaining:.0f}% left", used=r.used * 100, remaining=(1 - r.used) * 100))
            meter = "█" * filled + "░" * (bar_width - filled) + percentages
        reset = reset_text(r.reset, now) if r.reset else r.reset_label or reset_text(r.reset, now)
        if r.reset and r.reset > now:
            reset += time.strftime(" (%a %H:%M)" if r.reset - now > 86400 else " (%H:%M)", time.localtime(r.reset))
        reset += tr(' · stale') if stale else ""
        prefix = f"  {r.window:6} "
        if cols >= 120:
            label = clip(tr(r.label), 24)
            content.append((prefix + f"{label:<24}  {meter}  ·  {reset}", style))
        else:
            content.append((prefix + tr(r.label), "text"))
            content.append(("    " + meter, style))
            content.append(("    " + reset, "muted"))
        forecast = None if stale else pace(r, now)
        if forecast:
            content.append(((" " * 41 if cols >= 120 else "    ") + "↳ " + forecast[0], forecast[1]))

    for report in usage.reports:
        content.append(("", "text"))
        source = tr('agy CLI · cached') if report.cached else tr('agy CLI · live')
        content.extend((text, "cyan") for text in wrap(PROVIDERS[report.provider] + " · " + source, cols - 4))
        content.extend((text, "muted") for text in wrap(snapshot_text(report.updated), cols - 4))
        if now - report.updated > 3600:
            content.append((tr(' · stale').strip(), "amber"))
        content.extend((text, "text") for text in wrap(report.text, cols - 4))

    if not usage.loaded:
        content = [(tr('Loading local usage snapshots…'), "muted")]
    elif not usage.limits and not usage.reports:
        content = [(tr('No official CLI limits available.'), "text"),
                   (tr('Remaining usage cannot be calculated from session tokens.'), "muted")]
    if any(r.source.endswith(" · cached") for r in usage.limits) or any(r.cached for r in usage.reports):
        content.extend((text, "muted") for text in wrap(tr('Cached snapshots may belong to a previous CLI account.'), cols - 4))
    if usage.errors:
        content += [("", "text"), (tr('SOURCE ERRORS'), "amber")]
        for error in usage.errors:
            content.extend((text, "amber") for text in wrap(error, cols - 4))
    return content


def usage_viewport(cols: int, rows: int) -> int:
    header = 5 if rows >= 26 and cols >= 70 else 4
    return max(1, rows - 5 - (header + 5))


def max_usage_scroll(usage: Usage, cols: int, rows: int) -> int:
    return max(0, len(usage_content(usage, cols, time.time())) - usage_viewport(cols, rows))


def render_usage(base: Screen, usage: Usage, scroll: int = 0, refreshing: bool = False, message: str = "") -> Screen:
    cols, rows = base.cols, base.rows
    if cols < 44 or rows < 16:
        return base
    header = 5 if rows >= 26 and cols >= 70 else 4
    base.fill(header, 0, cols)
    base.put(header, 2, tr('◉ Official CLIs · current signed-in accounts'), "cyan", limit=cols - 4)
    for y in range(header + 2, rows):
        base.fill(y, 0, cols)
    base.put(header + 2, 2, tr('USAGE · PROVIDER LIMITS') if cols >= 70 else tr("USAGE · LIMITS"), "accent", bold=True)
    base.put(header + 3, 2, tr('Used and remaining · shared account quotas, independent of cwd'), "muted", limit=cols - 4)
    base.rule(header + 4)
    start, bottom = header + 5, rows - 5
    content = usage_content(usage, cols, time.time())
    scroll = max(0, min(scroll, max(0, len(content) - usage_viewport(cols, rows))))
    if len(content) > bottom - start:
        indicator = f"↑↓ / PgUp PgDn · {scroll+1}" if cols >= 80 else f"↑↓ {scroll+1}"
        base.put(header + 2, cols - width(indicator) - 2, indicator, "muted")
    for i, (text, style) in enumerate(content[scroll:]):
        if start + i >= bottom:
            break
        base.put(start + i, 2, text, style, limit=cols - 4)
    base.rule(rows - 5)
    base.put(rows - 4, 2, tr('↑↓ scroll   R refresh   rr restart   Esc / Backspace back   Q quit'), "text", limit=cols - 4)
    base.put(rows - 3, 2, tr('One account may be used by multiple CLIs; limits are not added together.'), "muted", limit=cols - 4)
    base.put(rows - 2, 2, tr('Without a snapshot, remaining usage is unknown.'), "muted", limit=cols - 4)
    footer = tr('Fetching current limits via official CLIs…') if refreshing else tr('Official CLI snapshots · R refresh')
    if usage.checked and not refreshing:
        footer = tr('Last check: {timestamp} UTC', timestamp=time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime(usage.checked)))
    base.put(rows - 1, 2, message or footer, "amber" if message or refreshing else "faint", limit=cols - 4)
    return base
