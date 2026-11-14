#!/usr/bin/env python3
"""Convert Kass's i18next translation.json into the Fluent files the Rust UI reads.

While React screens remain, translation.json is still edited; rerun this to
bring crates/kass-i18n/locales/en in step (the kass-i18n parity test fails
until you do):

    scripts/translations-to-fluent.py app/src/i18n/locales/en/translation.json crates/kass-i18n/locales/en

usage: translations-to-fluent.py <translation.json> <out_dir>
Writes <out_dir>/<section>.ftl per top-level JSON section. Exits non-zero on
id collisions, incomplete plural sets or non-string leaves.
"""
import json, re, sys, os

PLURAL = ("zero", "one", "two", "few", "many", "other")
PLURAL_RE = re.compile(r"^(.*)_(%s)$" % "|".join(PLURAL))


def kebab(seg):
    out = []
    for i, ch in enumerate(seg):
        if ch == "_":
            out.append("-")
        elif ch.isupper():
            if i > 0 and out and out[-1] != "-":
                out.append("-")
            out.append(ch.lower())
        else:
            out.append(ch)
    return "".join(out)


# Keys whose scheme id collides with another key's. Mirrors ID_OVERRIDES in
# crates/kass-i18n/src/id.rs.
OVERRIDES = {
    # `settings.features.onboarding.title` already takes the scheme id.
    "settings.features.onboardingTitle": "settings-features-onboarding-section-title",
}


def fluent_id(key):
    if key in OVERRIDES:
        return OVERRIDES[key]
    segs = key.split(".")
    m = PLURAL_RE.match(segs[-1])
    if m:
        segs[-1] = m.group(1)
    return "-".join(kebab(s) for s in segs)


def lit(s):
    return '{ "' + s.replace("\\", "\\\\").replace('"', '\\"') + '" }'


VAR_RE = re.compile(r"\{\{\s*([^{}]*?)\s*\}\}")
IDENT_RE = re.compile(r"^[A-Za-z][A-Za-z0-9_-]*$")


def escape_text(text, at_line_start, at_line_end):
    """Escape literal text so Fluent reproduces it exactly."""
    if text == "":
        return ""
    lead = len(text) - len(text.lstrip(" ")) if at_line_start else 0
    trail = len(text) - len(text.rstrip(" ")) if at_line_end else 0
    if lead == len(text):
        return lit(text)
    core = text[lead:len(text) - trail]
    # braces would open/close placeables
    core = re.sub(r"[{}]", lambda m: lit(m.group(0)), core)
    # `[`, `*`, `.` at the start of a line would read as a variant/attribute
    if at_line_start and not lead and core[:1] in ("[", "*", "."):
        core = lit(core[0]) + core[1:]
    return (lit(" " * lead) if lead else "") + core + (lit(" " * trail) if trail else "")


def escape_line(line):
    if line == "":
        return lit("")
    out, pos = "", 0
    for m in VAR_RE.finditer(line):
        name = m.group(1)
        if not IDENT_RE.match(name):
            raise SystemExit(f"variable {name!r} is not a valid Fluent identifier")
        out += escape_text(line[pos:m.start()], pos == 0, False)
        out += "{ $" + name + " }"
        pos = m.end()
    out += escape_text(line[pos:], pos == 0, True)
    return out


def pattern(value, indent):
    """Return text placed after `= ` / `] ` (may span lines)."""
    if value == "":
        return lit("")
    lines = value.split("\n")
    esc = [escape_line(l) for l in lines]
    if len(esc) == 1:
        return esc[0]
    pad = " " * indent
    return "\n" + "\n".join(pad + l for l in esc)


def main():
    src, out_dir = sys.argv[1], sys.argv[2]
    data = json.load(open(src, encoding="utf-8"))
    os.makedirs(out_dir, exist_ok=True)
    seen = {}  # fluent id -> json key (for collisions)
    errors = []
    for section, tree in data.items():
        leaves = []

        def walk(o, path):
            if isinstance(o, dict):
                for k, v in o.items():
                    walk(v, path + [k])
            elif isinstance(o, str):
                leaves.append((".".join(path), o))
            else:
                errors.append(f"non-string leaf {'.'.join(path)}: {o!r}")

        walk(tree, [section])
        # group plural sets, keep first-appearance order
        entries, groups = [], {}
        for key, val in leaves:
            m = PLURAL_RE.match(key.split(".")[-1])
            if m:
                base = key[: key.rfind("_")]
                if base not in groups:
                    groups[base] = {}
                    entries.append(("plural", base))
                groups[base][m.group(2)] = val
            else:
                entries.append(("plain", key, val))
        lines = [f"### Kass UI strings, section \"{section}\" (converted from i18next).", ""]
        for e in entries:
            if e[0] == "plain":
                _, key, val = e
                fid = fluent_id(key)
                text = f"{fid} = {pattern(val, 4)}"
                if text.startswith(f"{fid} = \n"):
                    text = f"{fid} ={pattern(val, 4)}"
            else:
                key = e[1]
                forms = groups[key]
                fid = fluent_id(key)
                if "other" not in forms:
                    errors.append(f"plural set {key} has no _other form")
                    continue
                variants = []
                for cat in PLURAL:
                    if cat not in forms:
                        continue
                    # i18next uses key_zero for count == 0 in every language;
                    # Fluent's exact-number variant [0] is the same rule.
                    name = "0" if cat == "zero" else cat
                    star = "*" if cat == "other" else " "
                    variants.append(f"       {star}[{name}] {pattern(forms[cat], 12)}".replace("] \n", "]\n"))
                text = f"{fid} =\n    {{ $count ->\n" + "\n".join(variants) + "\n    }"
            if fid in seen:
                errors.append(f"id collision: {key} and {seen[fid]} both map to {fid}")
            seen[fid] = key
            lines.append(text)
        with open(os.path.join(out_dir, kebab(section) + ".ftl"), "w", encoding="utf-8") as f:
            f.write("\n".join(lines) + "\n")
    if errors:
        print("\n".join(errors), file=sys.stderr)
        sys.exit(1)
    print(f"wrote {len(data)} files, {len(seen)} messages")


if __name__ == "__main__":
    main()
