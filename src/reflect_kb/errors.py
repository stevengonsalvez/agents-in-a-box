"""Lightweight error sink for the reflect pipeline.

Single bounded JSON file at $REFLECT_STATE_DIR/errors.json. Designed to be
called from both Python and bash (via `python -m reflect_kb.errors append ...`).
"""
import argparse, fcntl, hashlib, json, os, sys, tempfile
from datetime import datetime, timezone
from pathlib import Path

MAX_RECORDS = 200
DEDUPE_WINDOW_SEC = 86400  # 24h

def _state_dir() -> Path:
    return Path(os.environ.get("REFLECT_STATE_DIR", str(Path.home() / ".reflect"))).expanduser()

def _path() -> Path:
    return _state_dir() / "errors.json"

def _lock_path() -> Path:
    return _state_dir() / "errors.lock"

def _load() -> dict:
    p = _path()
    if not p.exists():
        return {"version": 1, "updated_at": None, "errors": []}
    try:
        return json.loads(p.read_text())
    except Exception:
        return {"version": 1, "updated_at": None, "errors": []}

def _save(doc: dict) -> None:
    p = _path()
    p.parent.mkdir(parents=True, exist_ok=True)
    tmp = tempfile.NamedTemporaryFile("w", dir=p.parent, delete=False, suffix=".tmp")
    json.dump(doc, tmp, indent=2)
    tmp.flush(); os.fsync(tmp.fileno()); tmp.close()
    os.replace(tmp.name, p)

def _now_iso() -> str:
    return datetime.now(timezone.utc).isoformat(timespec="seconds").replace("+00:00", "Z")

def append(severity: str, source: str, kind: str, message: str, context: dict | None = None) -> str:
    ts = _now_iso()
    key = f"{source}|{kind}|{message}"
    id_ = "err-" + hashlib.sha1(key.encode()).hexdigest()[:6]

    _state_dir().mkdir(parents=True, exist_ok=True)
    with open(_lock_path(), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        try:
            doc = _load()
            now = datetime.now(timezone.utc).timestamp()
            for rec in doc["errors"]:
                if rec["id"] != id_:
                    continue
                try:
                    age = now - datetime.fromisoformat(rec["ts"].replace("Z","+00:00")).timestamp()
                except Exception:
                    age = DEDUPE_WINDOW_SEC + 1
                if age < DEDUPE_WINDOW_SEC:
                    rec["count"] = rec.get("count", 1) + 1
                    rec["ts"] = ts
                    rec["acked"] = False
                    doc["updated_at"] = ts
                    _save(doc)
                    return id_

            doc["errors"].insert(0, {
                "id": id_, "ts": ts, "severity": severity, "source": source,
                "kind": kind, "message": message[:500], "context": context or {},
                "count": 1, "acked": False,
            })
            doc["errors"] = doc["errors"][:MAX_RECORDS]
            doc["updated_at"] = ts
            _save(doc)
            return id_
        finally:
            fcntl.flock(lock, fcntl.LOCK_UN)

def ack(ids: list[str] | None = None) -> int:
    with open(_lock_path(), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        doc = _load()
        n = 0
        for rec in doc["errors"]:
            if ids is None or rec["id"] in ids:
                if not rec.get("acked"):
                    rec["acked"] = True
                    n += 1
        doc["updated_at"] = _now_iso()
        _save(doc)
        fcntl.flock(lock, fcntl.LOCK_UN)
        return n

def count_unacked() -> int:
    return sum(1 for r in _load()["errors"] if not r.get("acked"))

def _cli():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(dest="cmd", required=True)
    a = sub.add_parser("append")
    a.add_argument("--severity", default="error", choices=["error","warn","info"])
    a.add_argument("--source", required=True)
    a.add_argument("--kind", required=True)
    a.add_argument("--message", required=True)
    a.add_argument("--context", default="{}")
    sub.add_parser("count")
    k = sub.add_parser("ack")
    k.add_argument("ids", nargs="*")
    args = p.parse_args()
    if args.cmd == "append":
        try: ctx = json.loads(args.context)
        except: ctx = {}
        print(append(args.severity, args.source, args.kind, args.message, ctx))
    elif args.cmd == "count":
        print(count_unacked())
    elif args.cmd == "ack":
        print(ack(args.ids or None))

if __name__ == "__main__":
    _cli()
