#!/usr/bin/env python3
"""Writes the 50 MB JSON fixtures the resource budgets use. Usage: fixtures.py OUT_DIR"""
import json, os, sys

SIZE = 50 * 1024 * 1024


def array_of_objects(f):
    f.write("[")
    i, n = 0, 1
    while n < SIZE:
        rec = json.dumps({"id": i, "name": f"user {i}", "email": f"u{i}@example.com", "tags": ["a", "b"], "active": i % 2 == 0}, separators=(",", ":"))
        if i:
            f.write(",")
            n += 1
        f.write(rec)
        n += len(rec)
        i += 1
    f.write("]")


def deep_nesting(f):
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


def long_string(f):
    f.write('{"blob":"')
    f.write("x" * SIZE)
    f.write('"}')


out = sys.argv[1]
os.makedirs(out, exist_ok=True)
for name, gen in [("array.json", array_of_objects), ("nested.json", deep_nesting), ("string.json", long_string)]:
    with open(os.path.join(out, name), "w") as f:
        gen(f)
    with open(os.path.join(out, name)) as f:
        json.load(f)
    print(name, os.path.getsize(os.path.join(out, name)))
