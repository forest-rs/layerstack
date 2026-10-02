# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Extract selected MaterialX NodeDef interfaces using Python's XML reader.

This does not convert node graphs, implementations, looks or material bindings.
"""
import json
import math
from pathlib import Path
import sys
import xml.etree.ElementTree as ET

TYPES = {
    "boolean": "bool", "integer": "int", "float": "float",
    "color3": "color3f", "color4": "color4f", "vector2": "float2",
    "vector3": "float3", "vector4": "float4", "matrix33": "matrix3d",
    "matrix44": "matrix4d", "string": "string", "filename": "asset",
}
WIDTHS = {"color3": 3, "color4": 4, "vector2": 2, "vector3": 3,
          "vector4": 4, "matrix33": 9, "matrix44": 16}


def literal(kind, text):
    if kind == "boolean":
        if text not in ("true", "false"): raise ValueError("invalid boolean default")
        return text
    if kind == "integer":
        value = int(text)
        if not -(2**31) <= value < 2**31: raise ValueError("integer default outside int32")
        return str(value)
    if kind in ("string", "filename"):
        if kind == "filename":
            if "@" in text: raise ValueError("asset default contains @")
            return "@" + text + "@"
        return json.dumps(text, ensure_ascii=False)
    parts = [float(x.strip()) for x in text.split(",")]
    if len(parts) != WIDTHS.get(kind, 1) or not all(math.isfinite(x) for x in parts):
        raise ValueError("invalid numeric default")
    if kind == "float": return repr(parts[0])
    if kind in ("matrix33", "matrix44"):
        width = 3 if kind == "matrix33" else 4
        return "(" + ", ".join("(" + ", ".join(map(repr, parts[i:i+width])) + ")" for i in range(0, len(parts), width)) + ")"
    return "(" + ", ".join(map(repr, parts)) + ")"


def extract(path, selected):
    definitions, loaded, active = {}, set(), set()

    def load(path):
        path = path.resolve(strict=True)
        if path in active: raise ValueError("cyclic MaterialX include")
        if path in loaded: return
        if len(active) >= 64: raise ValueError("MaterialX include depth exceeds 64")
        active.add(path)
        text = path.read_text(encoding="utf8")
        if "<!DOCTYPE" in text or "<!ENTITY" in text: raise ValueError("XML declarations are unsupported")
        root = ET.fromstring(text)
        if root.tag != "materialx": raise ValueError("expected materialx root")
        for element in root:
            if element.tag == "{http://www.w3.org/2001/XInclude}include":
                if element.get("parse", "xml") != "xml" or element.get("xpointer"):
                    raise ValueError("only whole-file XML includes are supported")
                href = element.get("href", "")
                if not href or "://" in href: raise ValueError("expected a local include path")
                load(path.parent / href)
            elif element.tag == "nodedef":
                name = element.get("name")
                if not name or name in definitions: raise ValueError("duplicate or unnamed NodeDef")
                definitions[name] = element
        active.remove(path)
        loaded.add(path)

    load(path)
    if not selected: raise ValueError("select at least one NodeDef")
    if len(set(selected)) != len(selected): raise ValueError("duplicate NodeDef selection")

    def interface(name, ancestors=()):
        if name in ancestors: raise ValueError("cyclic NodeDef inheritance: " + name)
        if len(ancestors) >= 64: raise ValueError("NodeDef inheritance depth exceeds 64")
        if name not in definitions: raise ValueError("unknown NodeDef: " + name)
        node = definitions[name]
        ports, attrs = ({}, {})
        if node.get("inherit"):
            ports, attrs = interface(node.get("inherit"), ancestors + (name,))
        ports, attrs = {key: dict(value) for key, value in ports.items()}, dict(attrs)
        attrs.update(node.attrib)
        seen = set()
        for port in node:
            if port.tag not in ("input", "output"): continue
            key = (port.tag, port.get("name"))
            if not key[1] or key in seen: raise ValueError("duplicate or unnamed port in " + name)
            seen.add(key)
            previous = ports.get(key, {})
            if previous.get("type") and port.get("type") and previous["type"] != port.get("type"):
                raise ValueError("incompatible inherited port in " + name)
            ports[key] = dict(previous, **port.attrib)
        return ports, attrs

    lines = ["#usda 1.0"]
    for index, name in enumerate(selected):
        ports, attrs = interface(name)
        # Infer legacy single outputs after all inherited attributes are overlaid.
        if not any(key[0] == "output" for key in ports):
            if attrs.get("type") == "multioutput": raise ValueError("multioutput NodeDef has no outputs")
            ports[("output", "out")] = {"type": attrs.get("type", "")}
        lines += [f'def Shader "Node{index}" {{', "    uniform token info:id = " + json.dumps(name)]
        for (direction, port_name), port in sorted(ports.items()):
            kind = port.get("type")
            if kind not in TYPES: raise ValueError(f"{name}.{port_name}: unsupported MaterialX type {kind}")
            if port.get("defaultgeomprop") or port.get("defaultinput"):
                raise ValueError(f"{name}.{port_name}: nonliteral defaults are unsupported")
            if not all(c.isalnum() or c in "_:" for c in port_name) or port_name[0].isdigit():
                raise ValueError("invalid port name")
            namespace = "inputs" if direction == "input" else "outputs"
            declaration = f"    {TYPES[kind]} {namespace}:{port_name}"
            if "value" in port: declaration += " = " + literal(kind, port["value"])
            if port.get("doc"): declaration += " (doc = " + json.dumps(port["doc"], ensure_ascii=False) + ")"
            lines.append(declaration)
        lines.append("}")
    return "\n".join(lines)


if __name__ == "__main__":
    try:
        print(extract(Path(sys.argv[1]), sys.argv[2:]))
    except (OSError, ValueError, ET.ParseError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
