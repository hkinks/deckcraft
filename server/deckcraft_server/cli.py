"""`python -m deckcraft_server …`: user administration and the server."""

from __future__ import annotations

import argparse
import asyncio
import getpass
import sqlite3
import sys
from pathlib import Path

from .auth.local import hash_password
from .config import load
from .store import Store


def _password(from_stdin: bool) -> str:
    if from_stdin:
        return sys.stdin.readline().rstrip("\n")
    first = getpass.getpass("Password: ")
    if getpass.getpass("Repeat: ") != first:
        raise SystemExit("passwords differ")
    return first


def main(argv: list[str] | None = None) -> int:
    ap = argparse.ArgumentParser(prog="deckcraft_server")
    ap.add_argument("--config", type=Path, default=Path("config.toml"))
    sub = ap.add_subparsers(dest="cmd", required=True)
    users = sub.add_parser("users").add_subparsers(dest="action", required=True)
    add = users.add_parser("add")
    add.add_argument("name")
    add.add_argument("--admin", action="store_true")
    add.add_argument("--password-stdin", action="store_true")
    pw = users.add_parser("passwd")
    pw.add_argument("name")
    pw.add_argument("--password-stdin", action="store_true")
    dis = users.add_parser("disable")
    dis.add_argument("name")
    serve = sub.add_parser("serve")
    serve.add_argument("--host", default="127.0.0.1")
    serve.add_argument("--port", type=int, default=8770)
    args = ap.parse_args(argv)

    settings = load(args.config)
    store = Store(settings.db_path)
    asyncio.run(store.init())

    if args.cmd == "serve":
        import uvicorn

        from .app import create_app

        uvicorn.run(create_app(settings, store), host=args.host, port=args.port, ws_max_size=16 << 20)
        return 0

    if args.action == "add":
        if not (password := _password(args.password_stdin)):
            print("empty password", file=sys.stderr)
            return 1
        try:
            asyncio.run(store.add_user(args.name, hash_password(password), admin=args.admin))
        except sqlite3.IntegrityError:
            print(f"user {args.name} already exists", file=sys.stderr)
            return 1
        print(f"added {args.name}")
    elif args.action == "passwd":
        if asyncio.run(store.get_user(args.name)) is None:
            print(f"no user {args.name}", file=sys.stderr)
            return 1
        asyncio.run(store.set_password(args.name, hash_password(_password(args.password_stdin))))
        print(f"password changed for {args.name}")
    elif args.action == "disable":
        asyncio.run(store.disable(args.name))
        print(f"disabled {args.name}")
    return 0
