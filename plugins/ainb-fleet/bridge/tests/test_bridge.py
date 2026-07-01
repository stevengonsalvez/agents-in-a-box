"""Bridge handler: send-loop fault tolerance (HTML -> plain-text fallback)."""

from __future__ import annotations

import asyncio
import types as _t

import ainb_phone_bridge.bridge as bridge
from ainb_phone_bridge.config import BridgeConfig


def _config() -> BridgeConfig:
    return BridgeConfig(
        token="x",
        authorized_user_id=42,
        default_target=None,
        response_timeout=1,
        require_mention_in_groups=False,
        proxy_url=None,
    )


class _FakeUser:
    def __init__(self, uid: int, is_bot: bool = False) -> None:
        self.id = uid
        self.is_bot = is_bot


class _FakeChat:
    type = "private"


class _RecordingMessage:
    """A minimal aiogram-Message stand-in that records / fails answer() calls."""

    def __init__(self, text: str, *, fail_html: bool) -> None:
        self.text = text
        self.caption = None
        self.from_user = _FakeUser(42)
        self.chat = _FakeChat()
        self.reply_to_message = None
        self._fail_html = fail_html
        self.sent: list[tuple[str, object]] = []

    async def answer(self, text, parse_mode=None):
        if self._fail_html and parse_mode is not None:
            raise RuntimeError("Telegram 400: can't parse entities")
        self.sent.append((text, parse_mode))


def _handler(config, monkeypatch, reply_text):
    """Build the dispatcher and pull out the registered message handler."""

    async def fake_relay(_config, _text):
        return reply_text

    monkeypatch.setattr(bridge, "_relay", fake_relay)

    captured: dict[str, object] = {}

    class _FakeDispatcher:
        def message(self):
            def deco(fn):
                captured["fn"] = fn
                return fn

            return deco

    # raising=False: when aiogram is absent bridge.Dispatcher doesn't exist, and
    # the suite must still run without the optional dependency.
    monkeypatch.setattr(bridge, "Dispatcher", lambda: _FakeDispatcher(), raising=False)
    # ParseMode is referenced in the handler; provide a stand-in if aiogram is
    # absent so the test runs without the optional dependency.
    if not bridge.HAS_AIOGRAM:
        monkeypatch.setattr(bridge, "ParseMode", _t.SimpleNamespace(HTML="HTML"), raising=False)

    bridge.build_dispatcher(config, bot=None)
    return captured["fn"]


def test_handler_falls_back_to_plain_text_on_html_failure(monkeypatch):
    config = _config()
    handler = _handler(config, monkeypatch, reply_text="**hi** a < b & c")
    msg = _RecordingMessage("ping", fail_html=True)

    asyncio.run(handler(msg))

    # HTML attempts failed, but the user still received the raw text.
    assert msg.sent, "expected a plain-text fallback send"
    assert all(parse_mode is None for _t_, parse_mode in msg.sent)
    assert "".join(t for t, _ in msg.sent) == "**hi** a < b & c"


def test_handler_sends_html_on_success(monkeypatch):
    config = _config()
    handler = _handler(config, monkeypatch, reply_text="**hi**")
    msg = _RecordingMessage("ping", fail_html=False)

    asyncio.run(handler(msg))

    assert msg.sent == [("<b>hi</b>", bridge.ParseMode.HTML)]
