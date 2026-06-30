"""Tests for the privacy sanitizer.

This is the gate every byte crosses before it can leave the machine for a
GitHub issue, so the assertions are deliberately strict: the secret/PII MUST be
gone from the output, not merely flagged.
"""

from __future__ import annotations

import pytest

from reflect_kb.issues.sanitize import sanitize

# Token-shaped fixtures are ASSEMBLED at runtime from a prefix + a synthetic
# body so this source file never contains a verbatim secret-shaped literal
# (which trips GitHub push-protection). The assembled strings still exercise the
# sanitizer's regexes exactly as a real token would.
_BODY = "abcdef0123456789abcdef0123456789"


@pytest.mark.parametrize(
    "secret",
    [
        "sk-ant-" + "api03-" + _BODY,
        "sk-proj-" + _BODY,
        "sk-" + _BODY,
        "gh" + "p_" + "abcdefghijklmnopqrstuvwxyz0123456789",
        "gh" + "s_" + "abcdefghijklmnopqrstuvwxyz0123456789",
        "xox" + "b-" + "1111111111-abcdefghijklmnop",
        "AKIA" + "IOSFODNN7" + "EXAMPLE",
    ],
)
def test_known_secret_shapes_are_redacted(secret):
    out = sanitize(f"the token is {secret} ok").text
    assert secret not in out
    assert "REDACTED" in out


def test_telegram_token_redacted():
    token = "123456789:AAH" + "z" * 32
    out = sanitize(f"bot {token}").text
    assert token not in out
    assert "<REDACTED:telegram_token>" in out


def test_jwt_redacted():
    jwt = "eyJhbGciOi.eyJzdWIiOi.SflKxwRJSMeKKF2QT4"
    out = sanitize(f"auth {jwt}").text
    assert jwt not in out
    assert "<REDACTED:jwt>" in out


def test_private_key_block_redacted():
    pem = (
        "-----BEGIN RSA PRIVATE KEY-----\n"
        "MIIEowIBAAKCAQEAabcdef\nGHIJKLMNOP\n"
        "-----END RSA PRIVATE KEY-----"
    )
    out = sanitize(f"key:\n{pem}\nrest").text
    assert "MIIEowIBAA" not in out
    assert "<REDACTED:private_key>" in out


def test_generic_key_value_secret_redacted():
    out = sanitize("API_KEY=supersecretvalue12345").text
    assert "supersecretvalue12345" not in out
    assert "<REDACTED:generic_secret>" in out
    # The key name is preserved, only the value is stripped.
    assert "API_KEY=" in out


def test_email_redacted():
    out = sanitize("contact jonny@shotclubhouse.com please").text
    assert "jonny@shotclubhouse.com" not in out
    assert "<REDACTED:email>" in out


def test_ipv4_redacted():
    out = sanitize("server at 203.0.113.42 down").text
    assert "203.0.113.42" not in out
    assert "<REDACTED:ipv4>" in out


def test_home_paths_anonymized():
    out = sanitize("file /Users/stevie/secret/project/x.py here").text
    assert "/Users/stevie" not in out
    assert "/Users/<user>" in out

    out2 = sanitize("file /home/ashesh/x").text
    assert "/home/ashesh" not in out2
    assert "/home/<user>" in out2


def test_tmp_and_worktree_paths_anonymized():
    out = sanitize("scratch at /tmp/build-xyz/log and .worktrees/feat-branch/x").text
    assert "/tmp/build-xyz" not in out
    assert "/tmp/<scratch>" in out
    assert "feat-branch" not in out
    assert "worktrees/<branch>" in out


def test_uuid_and_long_hex_redacted():
    out = sanitize(
        "session aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee hash abcdef0123456789abcdef01"
    ).text
    assert "aaaaaaaa-bbbb" not in out
    assert "<REDACTED:uuid>" in out
    assert "abcdef0123456789abcdef01" not in out
    assert "<REDACTED:hash>" in out


def test_uppercase_and_mixed_case_long_hex_redacted_and_not_audited():
    token = "ABCDEF0123456789abcdef01"
    res = sanitize(f"trace token {token}")
    assert token not in res.text
    assert "<REDACTED:hash>" in res.text
    assert res.redactions.get("long_hex") == 1
    assert all(f["kind"] != "residual_long_hex" for f in res.audit)


def test_custom_maps_applied_first():
    out = sanitize("AcmeCorp had an outage", maps={"AcmeCorp": "<company>"}).text
    assert "AcmeCorp" not in out
    assert "<company>" in out


def test_redaction_tally_counts_kinds():
    res = sanitize("a@b.com and c@d.org and 10.0.0.1")
    assert res.redactions.get("email") == 2
    assert res.redactions.get("ipv4") == 1
    assert res.total_redactions >= 3


def test_clean_text_passes_through_unchanged():
    text = "The CLI crashes when the config file is missing a required field."
    res = sanitize(text)
    assert res.text == text
    assert res.total_redactions == 0


def test_audit_flags_residual_suspicious_without_mutating():
    # A base64-ish blob that doesn't match a known secret shape still gets
    # flagged for a human (but the text is not mutated by the audit).
    blob = "Zm9vYmFyYmF6" * 5  # 60 chars base64-ish
    res = sanitize(f"data: {blob}")
    kinds = {f["kind"] for f in res.audit}
    assert "possible_base64_blob" in kinds


def test_audit_ignores_own_redaction_placeholders():
    # The audit runs on ALREADY-redacted text. A fully-redacted env assignment
    # like AWS_SECRET_ACCESS_KEY=<REDACTED:generic_secret> must NOT be re-flagged
    # as a suspicious env_var_assignment (or base64 blob) — that noise would bury
    # genuine residue. A line whose only "suspicious" content is the sanitizer's
    # own placeholder produces zero audit findings.
    res = sanitize("export AWS_SECRET_ACCESS_KEY=" + _BODY + "wJalrXUtnFEMI")
    # Sanity: the value really was redacted to a placeholder.
    assert "<REDACTED:generic_secret>" in res.text
    # And the audit does not flag that placeholder line.
    assert res.audit == []


def test_audit_still_flags_genuine_residue_alongside_placeholders():
    # Masking placeholders must not blind the audit to REAL residue on the same
    # text: a redacted line plus a separate base64-ish blob still flags the blob.
    blob = "Zm9vYmFyYmF6" * 5  # 60 chars base64-ish
    res = sanitize("API_KEY=supersecretvalue12345\ndata: " + blob)
    kinds = {f["kind"] for f in res.audit}
    assert "possible_base64_blob" in kinds
    # The redacted first line contributes no env_var_assignment noise.
    assert all(f["line"] != 1 for f in res.audit)


def test_secret_wins_over_hash_classification():
    # A GitHub token is also a long alphanumeric run; it must be redacted as a
    # token, never weakened to <REDACTED:hash>. Prefix assembled at runtime so
    # no verbatim token literal lands in source.
    token = "gh" + "p_" + "a" * 36
    out = sanitize(token).text
    assert "<REDACTED:github_token>" in out
    assert "<REDACTED:hash>" not in out


@pytest.mark.parametrize(
    "assignment",
    [
        "AWS_SECRET_ACCESS_KEY=" + _BODY + "wJalrXUtnFEMI",
        "GITLAB_TOKEN=" + "glpat" + _BODY[:20],
        "GOOGLE_API_KEY=" + _BODY + "AIzaSyXYZ",
        "NPM_TOKEN=" + "npm_" + _BODY,
    ],
)
def test_embedded_keyword_env_vars_are_redacted(assignment):
    # The keyword (KEY/TOKEN) is embedded in a longer env-var name; ``_`` is a
    # word char so a strict ``\b(key)\b`` boundary never matches and the value
    # would leak. The relaxed surrounding-``\w*`` rule must catch them.
    name, _, value = assignment.partition("=")
    out = sanitize(f"export {assignment}").text
    assert value not in out
    assert "REDACTED" in out
    # The env-var name is preserved, only the value is stripped.
    assert name in out


def test_gitlab_pat_redacted():
    token = "glpat-" + "a" * 24
    out = sanitize(f"clone with {token} now").text
    assert token not in out
    assert "<REDACTED:gitlab_token>" in out


def test_google_api_key_redacted():
    key = "AIza" + "Sy" + "B" * 33
    out = sanitize(f"maps key {key}").text
    assert key not in out
    assert "<REDACTED:google_api_key>" in out


def test_npm_token_redacted():
    token = "npm_" + "z" * 36
    out = sanitize(f"//registry.npmjs.org/:_authToken={token}").text
    assert token not in out
    assert "<REDACTED:npm_token>" in out


def test_slack_webhook_url_redacted():
    url = "https://hooks.slack.com/services/T0000/B0000/abcdefghijklmnop"
    out = sanitize(f"posting to {url} ok").text
    assert "abcdefghijklmnop" not in out
    assert "<REDACTED:slack_webhook>" in out


def test_authorization_bearer_token_redacted():
    token = "opaqueBearerToken1234567890"
    out = sanitize(f"Authorization: Bearer {token}").text
    assert token not in out
    assert "<REDACTED:bearer_token>" in out
    # The scheme is preserved so the line still reads.
    assert "Bearer" in out


def test_fine_grained_github_pat_is_redacted():
    # Fine-grained PATs (github_pat_<22>_<59>) are NOT matched by the classic
    # gh[posru]_ rule (underscore mid-body, ``i`` after ``gh``) — a dedicated
    # pattern must catch them or a live credential leaks into a published
    # issue. Prefix assembled at runtime so no verbatim token lands in source.
    token = "github" + "_pat_" + "1" * 22 + "_" + "b" * 59
    out = sanitize(token).text
    assert token not in out
    assert "<REDACTED:github_token>" in out
