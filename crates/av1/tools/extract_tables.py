#!/usr/bin/env python3
"""Extract the constants and constant tables of the AV1 Bitstream & Decoding Process
Specification (v1.0.0 with Errata 1) from its Markdown source.

Usage:
    curl -L -o av1-spec.zip https://github.com/AOMediaCodec/av1-spec/archive/5e04f3f75e73a5898d7616c47c52f032144b8f80.zip
    unzip av1-spec.zip
    python3 extract_tables.py av1-spec-5e04f3f75e73a5898d7616c47c52f032144b8f80 > ../src/spec_tables.rs

Only the specification text is read:
  * named constants from the symbol table of section 3 and the value/name tables of section 6
    (e.g. `| 1 | BLOCK_4X8`);
  * every C table initialiser `Name[ dims ] = { ... }` in any section.
Each table is emitted as a nested Rust array whose shape is checked against the declared
dimensions; the element type is the narrowest of u8/i8/u16/i16/i32 that holds every value.
"""
import os
import ast
import operator
import re
import sys
from pathlib import Path


def _spec_path(arg):
    """Resolve a CLI path argument and require it to be an existing regular file."""
    path = Path(arg).resolve()
    if not path.is_file():
        sys.exit(f"not a readable file: {arg}")
    return path


root = sys.argv[1]
files = sorted(f for f in os.listdir(root) if f.endswith(".md"))
texts = {f: open(os.path.join(root, f), encoding="utf-8").read() for f in files}

consts = {}


def ev(expr):
    e = expr.strip().replace("\\", "")
    e = re.sub(r"\b([A-Z][A-Z0-9_]*)\b", lambda m: str(consts[m.group(1)]) if m.group(1) in consts else m.group(1), e)
    e = e.replace("/", "//")
    binary = {
        ast.Add: operator.add,
        ast.Sub: operator.sub,
        ast.Mult: operator.mul,
        ast.FloorDiv: operator.floordiv,
        ast.Mod: operator.mod,
        ast.LShift: operator.lshift,
        ast.RShift: operator.rshift,
        ast.BitOr: operator.or_,
        ast.BitAnd: operator.and_,
        ast.BitXor: operator.xor,
    }
    unary = {ast.UAdd: operator.pos, ast.USub: operator.neg, ast.Invert: operator.invert}

    def calculate(node):
        if isinstance(node, ast.Constant) and isinstance(node.value, int):
            return node.value
        if isinstance(node, ast.BinOp) and type(node.op) in binary:
            return binary[type(node.op)](calculate(node.left), calculate(node.right))
        if isinstance(node, ast.UnaryOp) and type(node.op) in unary:
            return unary[type(node.op)](calculate(node.operand))
        raise ValueError("unsupported specification expression")

    return int(calculate(ast.parse(e, mode="eval").body))


# --- constants: symbol table (section 3) ---
pending = []
for line in texts["03.symbols.md"].split("\n"):
    m = re.match(r"^\|\s*`([A-Z][A-Z0-9_]*)`\s*\|\s*([^|]+?)\s*\|", line)
    if m:
        pending.append((m.group(1), m.group(2)))
# --- constants: value/name tables (section 6 semantics, section 8/9) ---
# A row cell holding a NAME takes its value from the integer cell just before it, unless the
# next cell is another NAME (multi-column mapping tables such as the CFL sign table).
NAME = re.compile(r"^[A-Z][A-Z0-9_]*[A-Z0-9]$")
for f in files:
    for line in texts[f].split("\n"):
        if not line.startswith("|"):
            continue
        cells = [c.strip().rstrip(".") for c in line.strip().strip("|").split("|")]
        for k in range(1, len(cells)):
            if NAME.match(cells[k]) and len(cells[k]) >= 4 and re.match(r"^-?\d+$", cells[k - 1]):
                if k + 1 < len(cells) and NAME.match(cells[k + 1]) and "_" in cells[k + 1]:
                    continue
                pending.append((cells[k], cells[k - 1]))
        # `| NAME | value` rows
        if len(cells) >= 2 and NAME.match(cells[0]) and "_" in cells[0] and re.match(r"^-?\d+$", cells[1]):
            pending.append((cells[0], cells[1]))
for _ in range(10):
    rest = []
    for name, val in pending:
        try:
            v = ev(val)
        except Exception:
            rest.append((name, val))
            continue
        if name in consts and consts[name] != v:
            # same name, different meaning in two tables: keep the first (reported)
            sys.stderr.write("constant %s: %d vs %d (kept first)\n" % (name, consts[name], v))
            continue
        consts[name] = v
    pending = rest
for name, val in pending:
    sys.stderr.write("unresolved constant %s = %s\n" % (name, val))


# --- tables ---
def cells(text):
    """Split one initialiser cell. A missing comma between two operands on separate lines
    (Split_Tx_Size in the spec text: `TX_32X32` / `TX_4X8`) yields two cells."""
    parts = re.split(r"(?<=[A-Za-z0-9_])\s*\n\s*(?=[A-Za-z0-9_])", text.strip())
    if len(parts) > 1:
        sys.stderr.write("note: missing comma in spec table between %s\n" % parts)
    return parts


def parse_braces(s, i):
    """Parse a brace initialiser starting at s[i] == '{'; returns (nested list, end index)."""
    assert s[i] == "{"
    i += 1
    items = []
    cur = ""
    while True:
        ch = s[i]
        if ch == "{":
            sub, i = parse_braces(s, i)
            items.append(sub)
            cur = ""
            continue
        if ch == "}":
            if cur.strip():
                items.extend(cells(cur))
            return items, i + 1
        if ch == ",":
            if cur.strip():
                items.extend(cells(cur))
            cur = ""
        else:
            cur += ch
        i += 1


def shape(x):
    if isinstance(x, list):
        subs = [shape(e) for e in x]
        if any(s != subs[0] for s in subs):
            return None
        return [len(x)] + (subs[0] if subs else [])
    return []


tables = {}
for f in files:
    t = texts[f]
    for block in re.findall(r"~~~~~?\s*c?\n(.*?)~~~~~?", t, re.S):
        b = re.sub(r"//[^\n]*", "", block)
        b = re.sub(r"/\*.*?\*/", "", b, flags=re.S)
        for m in re.finditer(r"(?m)^\s*([A-Z][A-Za-z0-9_]*)\s*((?:\[[^\]=]*\]\s*)+)=\s*", b):
            name = m.group(1)
            try:
                dims = [ev(d) for d in re.findall(r"\[([^\]]*)\]", m.group(2))]
            except Exception:
                continue  # an assignment in pseudo code, not a table
            j = b.find("{", m.end())
            if j < 0 or b[m.end():j].strip():
                continue
            val, _ = parse_braces(b, j)
            sh = shape(val)
            if sh != dims:
                sys.stderr.write("table %s: shape %s != declared %s (skipped)\n" % (name, sh, dims))
                continue
            flat = []

            def walk(x):
                if isinstance(x, list):
                    out = []
                    for e in x:
                        out.append(walk(e))
                    return out
                v = ev(x)
                flat.append(v)
                return v

            nested = walk(val)
            if name in tables and tables[name][1] != nested:
                sys.stderr.write("table %s defined twice with different values\n" % name)
            tables[name] = (dims, nested, flat)


def rust_type(flat):
    lo, hi = min(flat), max(flat)
    for t, a, b in (("u8", 0, 255), ("i8", -128, 127), ("u16", 0, 65535), ("i16", -32768, 32767)):
        if lo >= a and hi <= b:
            return t
    return "i32"


def fmt(x, depth):
    if isinstance(x, list) and x and isinstance(x[0], list):
        inner = ",\n".join("    " * (depth + 1) + fmt(e, depth + 1) for e in x)
        return "[\n" + inner + ",\n" + "    " * depth + "]"
    return "[" + ", ".join(str(v) for v in x) + "]"


out = [
    "// Generated by tools/extract_tables.py from the AV1 Bitstream & Decoding Process Specification",
    "// v1.0.0 with Errata 1 (AOMediaCodec/av1-spec 5e04f3f). Do not edit by hand.",
    "#![allow(dead_code, clippy::unreadable_literal, clippy::excessive_precision)]",
    "",
]
for name in sorted(consts):
    v = consts[name]
    out.append("pub const %s: %s = %d;" % (name, "i32" if v < 0 else "usize", v))
out.append("")
for name in sorted(tables):
    dims, nested, flat = tables[name]
    # scan tables share one element type so they can be returned as &[u16]
    t = "u16" if "_Scan_" in name else rust_type(flat)
    rname = name.upper()
    if rname in consts:
        rname += "_TABLE"  # e.g. Max_Tx_Depth (table) vs MAX_TX_DEPTH (constant)
    ty = t
    for d in reversed(dims):
        ty = "[%s; %d]" % (ty, d)
    out.append("pub static %s: %s = %s;" % (rname, ty, fmt(nested, 0)))
    out.append("")
print("\n".join(out))
sys.stderr.write("%d constants, %d tables\n" % (len(consts), len(tables)))
