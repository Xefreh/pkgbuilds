"""Telegram notification: a single digest message per run."""

from __future__ import annotations

import json
import os
import urllib.parse
import urllib.request
from dataclasses import dataclass


@dataclass
class PkgResult:
    name: str
    status: str          # UPDATED / UP_TO_DATE / BROKEN / FAILED / WARN
    detail: str = ""

    def render(self) -> str:
        icon = {
            "UPDATED": "✅",
            "UP_TO_DATE": "⏸",
            "BROKEN": "⚠️",
            "FAILED": "❌",
            "WARN": "🟡",
        }.get(self.status, "•")
        line = f"{icon} {self.name}: {self.status.lower().replace('_', ' ')}"
        if self.detail:
            line += f" ({self.detail})"
        return line


class TelegramNotifier:
    """Sends a digest to a single chat via the Bot API. No-op if disabled."""

    def __init__(self) -> None:
        self.token = os.environ.get("TG_BOT_TOKEN", "")
        self.chat_id = os.environ.get("TG_CHAT_ID", "")
        self.enabled = bool(self.token and self.chat_id)

    def send(self, text: str) -> None:
        if not self.enabled:
            return
        url = f"https://api.telegram.org/bot{self.token}/sendMessage"
        payload = urllib.parse.urlencode({
            "chat_id": self.chat_id,
            "text": text,
            "disable_web_page_preview": "true",
        }).encode()
        req = urllib.request.Request(url, data=payload,
                                     headers={"Content-Type":
                                              "application/x-www-form-urlencoded"})
        try:
            with urllib.request.urlopen(req, timeout=30) as r:
                data = json.loads(r.read())
            if not data.get("ok"):
                print(f"[telegram] API error: {data}")
        except Exception as exc:  # network failures must not abort the run
            print(f"[telegram] send failed: {exc}")

    def send_digest(self, results: list[PkgResult], run_id: str) -> None:
        if not results:
            return
        header = f"🖥 AUR updater — {run_id}"
        body = "\n".join(r.render() for r in results)
        self.send(f"{header}\n{body}")
