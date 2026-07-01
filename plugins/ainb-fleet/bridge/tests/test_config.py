"""Config parsing + validation, including secret-resolved token/user_id."""

from __future__ import annotations

import pytest

from ainb_phone_bridge.config import BridgeConfig, ConfigError, load_config, parse_config


def test_parse_minimal():
    cfg = parse_config({"fleet": {"telegram": {"token": "123:ABC", "user_id": 42}}})
    assert isinstance(cfg, BridgeConfig)
    assert cfg.token == "123:ABC"
    assert cfg.authorized_user_id == 42
    assert cfg.require_mention_in_groups is True
    assert cfg.response_timeout == 300


def test_parse_full():
    cfg = parse_config(
        {
            "fleet": {
                "telegram": {
                    "token": "123:ABC",
                    "user_id": 7,
                    "default_target": "conductor",
                    "require_mention_in_groups": False,
                    "response_timeout": 120,
                    "proxy_url": "http://proxy:8080",
                }
            }
        }
    )
    assert cfg.default_target == "conductor"
    assert cfg.require_mention_in_groups is False
    assert cfg.response_timeout == 120
    assert cfg.proxy_url == "http://proxy:8080"


def test_token_from_env(monkeypatch):
    monkeypatch.setenv("TG_TOKEN", "env-token")
    cfg = parse_config({"fleet": {"telegram": {"token": "$TG_TOKEN", "user_id": 1}}})
    assert cfg.token == "env-token"


def test_user_id_as_env_string(monkeypatch):
    monkeypatch.setenv("TG_UID", "999")
    cfg = parse_config({"fleet": {"telegram": {"token": "t", "user_id": "$TG_UID"}}})
    assert cfg.authorized_user_id == 999


def test_missing_fleet_table():
    with pytest.raises(ConfigError, match="no .fleet."):
        parse_config({})


def test_missing_telegram_table():
    with pytest.raises(ConfigError, match="telegram"):
        parse_config({"fleet": {}})


def test_missing_token():
    with pytest.raises(ConfigError, match="token"):
        parse_config({"fleet": {"telegram": {"user_id": 1}}})


def test_missing_user_id():
    with pytest.raises(ConfigError, match="user_id"):
        parse_config({"fleet": {"telegram": {"token": "t"}}})


def test_empty_token_after_resolution(monkeypatch):
    monkeypatch.delenv("UNSET_TG", raising=False)
    with pytest.raises(ConfigError, match="empty"):
        parse_config({"fleet": {"telegram": {"token": "$UNSET_TG", "user_id": 1}}})


def test_bool_user_id_rejected():
    with pytest.raises(ConfigError, match="boolean"):
        parse_config({"fleet": {"telegram": {"token": "t", "user_id": True}}})


def test_non_numeric_user_id_string(monkeypatch):
    monkeypatch.setenv("BAD_UID", "notanumber")
    with pytest.raises(ConfigError, match="not an integer"):
        parse_config({"fleet": {"telegram": {"token": "t", "user_id": "$BAD_UID"}}})


def test_response_timeout_must_be_positive():
    with pytest.raises(ConfigError, match="positive"):
        parse_config({"fleet": {"telegram": {"token": "t", "user_id": 1, "response_timeout": 0}}})


def test_load_config_from_file(tmp_path):
    cfg_file = tmp_path / "config.toml"
    cfg_file.write_text('[fleet.telegram]\ntoken = "abc:123"\nuser_id = 555\n', encoding="utf-8")
    cfg = load_config(cfg_file)
    assert cfg.token == "abc:123"
    assert cfg.authorized_user_id == 555


def test_load_config_missing_file(tmp_path):
    with pytest.raises(ConfigError, match="not found"):
        load_config(tmp_path / "nope.toml")
