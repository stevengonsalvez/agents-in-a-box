# ABOUTME: The aiogram v3 Telegram daemon — long-polling, two-way relay.
#
# Inbound: an authorized Telegram message -> resolve target session (prefix
# "name: msg" selects a named session, bare text hits the conductor/default) ->
# send via tmux + capture the session's end-of-turn reply -> reply to Telegram
# as HTML, split at 4096 chars.
#
# Authorization is by Telegram user_id (unknown senders are silently ignored).
# In group chats, the bot only acts when @mentioned or when the message is a
# reply to the bot (configurable). Blocking subprocess work is offloaded to a
# thread so the asyncio event loop is never blocked.
#
# aiogram is an optional import: the module is import-safe (for tests / install)
# without it; `run_bridge` raises a clear error if it is genuinely missing.

from __future__ import annotations

import asyncio
import functools
import logging

from . import __version__
from .ainb_client import discover, send_and_capture
from .config import BridgeConfig, load_config
from .format import md_to_tg_html, split_message
from .routing import parse_target_prefix, resolve_target

log = logging.getLogger("ainb.bridge")

try:  # aiogram is only needed to actually run the daemon.
    from aiogram import Bot, Dispatcher, types
    from aiogram.client.default import DefaultBotProperties
    from aiogram.enums import ParseMode

    HAS_AIOGRAM = True
except ImportError:  # pragma: no cover - exercised only without the dep
    HAS_AIOGRAM = False


def _strip_bot_mention(text: str, username: str | None) -> str:
    """Remove a leading ``@botname`` mention from group messages."""
    if not username:
        return text
    handle = f"@{username}"
    stripped = text.strip()
    if stripped.lower().startswith(handle.lower()):
        return stripped[len(handle) :].strip()
    return text


async def _relay(config: BridgeConfig, raw_text: str) -> str:
    """Resolve the target, relay the message, and return a user-facing reply.

    Pure-ish orchestration: all blocking ainb/tmux/transcript work happens in a
    worker thread via ``run_in_executor`` so the event loop stays responsive.
    """
    loop = asyncio.get_running_loop()
    sessions = await loop.run_in_executor(None, discover)

    if not sessions:
        return "No running ainb sessions to relay to."

    names = [s.name for s in sessions]
    parsed_name, message = parse_target_prefix(raw_text, names)

    # No explicit prefix -> honour a configured default target name if present.
    if parsed_name is None and config.default_target:
        if any(s.name.lower() == config.default_target.lower() for s in sessions):
            parsed_name = config.default_target

    target = resolve_target(parsed_name, sessions)
    if target is None:
        if parsed_name is not None:
            return f"No running session named {parsed_name!r}."
        return "No target session available."

    if not message:
        return f"(empty message — nothing sent to {target.name})"

    reply = await loop.run_in_executor(
        None,
        functools.partial(send_and_capture, target, message, float(config.response_timeout)),
    )
    if reply is None:
        return (
            f"Sent to {target.name}, but no reply within "
            f"{config.response_timeout}s (it may still be working)."
        )
    return reply


def build_dispatcher(config: BridgeConfig, bot: Bot) -> Dispatcher:
    """Wire the aiogram dispatcher with authorization + relay handlers."""
    dp = Dispatcher()
    bot_state: dict[str, str | None] = {"username": None}

    async def _ensure_username() -> str | None:
        if bot_state["username"] is None:
            try:
                me = await bot.get_me()
                bot_state["username"] = me.username
            except Exception:  # pragma: no cover - network
                bot_state["username"] = None
        return bot_state["username"]

    def _is_authorized(message: types.Message) -> bool:
        return bool(message.from_user and message.from_user.id == config.authorized_user_id)

    async def _is_addressed(message: types.Message) -> bool:
        if message.chat.type == "private":
            return True
        if not config.require_mention_in_groups:
            return True
        # Reply to the bot's own message counts as addressed.
        if (
            message.reply_to_message
            and message.reply_to_message.from_user
            and message.reply_to_message.from_user.is_bot
        ):
            return True
        username = await _ensure_username()
        if username and message.text:
            return f"@{username}".lower() in message.text.lower()
        return False

    @dp.message()
    async def handle_message(message: types.Message) -> None:
        if not _is_authorized(message):
            log.warning(
                "ignoring message from unauthorized user_id=%s",
                getattr(message.from_user, "id", None),
            )
            return
        if not await _is_addressed(message):
            return

        text = message.text or message.caption or ""
        username = await _ensure_username()
        text = _strip_bot_mention(text, username)
        if not text.strip():
            return

        try:
            reply = await _relay(config, text)
        except Exception as exc:  # pragma: no cover - defensive
            log.exception("relay failed")
            reply = f"Bridge error: {exc}"

        html = md_to_tg_html(reply)
        for chunk in split_message(html):
            await message.answer(chunk, parse_mode=ParseMode.HTML)

    return dp


async def _amain(config: BridgeConfig) -> None:
    session = None
    if config.proxy_url:
        from aiogram.client.session.aiohttp import AiohttpSession

        session = AiohttpSession(proxy=config.proxy_url)
    bot = Bot(
        token=config.token,
        session=session,
        default=DefaultBotProperties(parse_mode=ParseMode.HTML),
    )
    dp = build_dispatcher(config, bot)
    log.info("ainb phone bridge v%s starting (long-polling)", __version__)
    try:
        await dp.start_polling(bot)
    finally:
        await bot.session.close()


def run_bridge(config: BridgeConfig | None = None) -> int:
    """Entry point for the daemon. Loads config if not supplied. Returns 0."""
    if not HAS_AIOGRAM:
        log.error(
            "aiogram is not installed — run `pip install -r requirements.txt` "
            "in the bridge venv (see README)."
        )
        return 1
    cfg = config or load_config()
    try:
        asyncio.run(_amain(cfg))
    except (KeyboardInterrupt, SystemExit):
        log.info("bridge stopped")
    return 0
