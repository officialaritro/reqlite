#!/usr/bin/env python3
"""Writes the 50 MB JSON and XML fixtures the resource budgets use. Usage: fixtures.py OUT_DIR"""

import json
import os
import sys
from typing import TextIO

SIZE = 50 * 1024 * 1024


def array_of_objects(f: TextIO) -> None:
    f.write("[")
    i, n = 0, 1
    while n < SIZE:
        rec = json.dumps(
            {
                "id": i,
                "name": f"user {i}",
                "email": f"u{i}@example.com",
                "tags": ["a", "b"],
                "active": i % 2 == 0,
            },
            separators=(",", ":"),
        )
        if i:
            f.write(",")
            n += 1
        f.write(rec)
        n += len(rec)
        i += 1
    f.write("]")


def deep_nesting(f: TextIO) -> None:
    rec = {"leaf": [1, 2, 3], "name": "deep"}
    for d in range(20):
        rec = {"level": d, "child": rec}
    rec = json.dumps(rec, separators=(",", ":"))
    f.write("[")
    n = 1
    while n < SIZE:
        if n > 1:
            f.write(",")
        f.write(rec)
        n += len(rec) + 1
    f.write("]")


def long_string(f: TextIO) -> None:
    f.write('{"blob":"')
    f.write("x" * SIZE)
    f.write('"}')


def markup(f: TextIO) -> None:
    f.write('<?xml version="1.0"?><users>')
    i, n = 0, 0
    while n < SIZE:
        rec = f'<user id="{i}"><name>user {i}</name><email>u{i}@example.com</email><active/></user>'
        f.write(rec)
        n += len(rec)
        i += 1
    f.write("</users>")


out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for name, gen in [
    ("array.json", array_of_objects),
    ("nested.json", deep_nesting),
    ("string.json", long_string),
    ("markup.xml", markup),
]:
    with open(os.path.join(out, name), "w") as f:
        gen(f)
    if name.endswith(".json"):
        with open(os.path.join(out, name)) as f:
            json.load(f)
    print(name, os.path.getsize(os.path.join(out, name)))
