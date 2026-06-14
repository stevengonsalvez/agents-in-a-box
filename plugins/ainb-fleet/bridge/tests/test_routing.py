"""Prefix parsing + target resolution (conductor-first, degrade-to-any)."""

from __future__ import annotations

from ainb_phone_bridge.routing import (
    TargetSession,
    default_target,
    is_conductor_name,
    parse_target_prefix,
    resolve_target,
)


def mk(name, *, conductor=False):
    return TargetSession(
        name=name,
        tmux_session=f"tmux_{name}",
        cwd=f"/work/{name}",
        session_id=f"id-{name}",
        is_conductor=conductor,
    )


# --- parse_target_prefix ---------------------------------------------------


def test_prefix_selects_known_name():
    name, msg = parse_target_prefix("ryan: hello there", ["ryan", "ana"])
    assert name == "ryan"
    assert msg == "hello there"


def test_prefix_case_insensitive():
    name, msg = parse_target_prefix("RYAN: hi", ["ryan"])
    assert name == "ryan"
    assert msg == "hi"


def test_unknown_prefix_treated_as_plain_text():
    name, msg = parse_target_prefix("note: buy milk", ["ryan"])
    assert name is None
    assert msg == "note: buy milk"


def test_bare_message_no_prefix():
    name, msg = parse_target_prefix("just do the thing", ["ryan"])
    assert name is None
    assert msg == "just do the thing"


def test_empty_message():
    assert parse_target_prefix("", ["ryan"]) == (None, "")


def test_prefix_with_multiline_body():
    name, msg = parse_target_prefix("ryan: line1\nline2", ["ryan"])
    assert name == "ryan"
    assert msg == "line1\nline2"


# --- is_conductor_name -----------------------------------------------------


def test_is_conductor_name():
    assert is_conductor_name("conductor")
    assert is_conductor_name("conductor-main")
    assert not is_conductor_name("ryan")


# --- default_target / resolve_target --------------------------------------


def test_default_prefers_conductor():
    sessions = [mk("ana"), mk("conductor-main", conductor=True), mk("zed")]
    assert default_target(sessions).name == "conductor-main"


def test_default_degrades_to_alphabetical_when_no_conductor():
    sessions = [mk("zed"), mk("ana"), mk("ben")]
    assert default_target(sessions).name == "ana"


def test_default_empty_is_none():
    assert default_target([]) is None


def test_resolve_named_target_exact():
    sessions = [mk("ana"), mk("ben")]
    assert resolve_target("ben", sessions).name == "ben"


def test_resolve_named_target_missing_returns_none():
    sessions = [mk("ana"), mk("ben")]
    assert resolve_target("zed", sessions) is None


def test_resolve_no_name_uses_default():
    sessions = [mk("ana"), mk("conductor", conductor=True)]
    assert resolve_target(None, sessions).name == "conductor"


def test_resolve_degrades_to_any_session():
    # No conductor present — the bridge must still relay (criterion: useful
    # before the conductor track lands).
    sessions = [mk("solo")]
    assert resolve_target(None, sessions).name == "solo"
