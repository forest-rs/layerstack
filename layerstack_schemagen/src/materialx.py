# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Extract selected MaterialX NodeDef interfaces using Python's XML reader.

This does not convert node graphs, implementations, looks or material bindings.
"""
import json
from pathlib import Path
import sys
import xml.etree.ElementTree as ET


def extract(path, selected):
    definitions, loaded, active = {}, set(), set()

    def load(path):
        path = path.resolve(strict=True)
        if path in active:
            raise ValueError(f"cyclic MaterialX include: {path}")
        if path in loaded:
            return
        if len(active) >= 64:
            raise ValueError("MaterialX include depth exceeds 64")
        active.add(path)
        text = path.read_text(encoding="utf8")
        if "<!DOCTYPE" in text or "<!ENTITY" in text:
            raise ValueError("XML declarations are unsupported")
        root = ET.fromstring(text)
        if root.tag != "materialx":
            raise ValueError("expected materialx root")
        for element in root:
            if element.tag == "{http://www.w3.org/2001/XInclude}include":
                if element.get("parse", "xml") != "xml" or element.get("xpointer"):
                    raise ValueError("only whole-file XML includes are supported")
                href = element.get("href", "")
                if not href or "://" in href:
                    raise ValueError("expected a local include path")
                load(path.parent / href)
            elif element.tag == "nodedef":
                name = element.get("name")
                if not name or name in definitions:
                    raise ValueError(f"{path}: duplicate or unnamed NodeDef {name!r}")
                definitions[name] = element
        active.remove(path)
        loaded.add(path)

    load(path)
    if not selected:
        raise ValueError("select at least one NodeDef")
    if len(set(selected)) != len(selected):
        raise ValueError("duplicate NodeDef selection")

    def interface(name, ancestors=()):
        if name in ancestors:
            raise ValueError("cyclic NodeDef inheritance: " + name)
        if len(ancestors) >= 64:
            raise ValueError("NodeDef inheritance depth exceeds 64")
        if name not in definitions:
            raise ValueError("unknown NodeDef: " + name)
        node = definitions[name]
        ports, attrs = {}, {}
        if node.get("inherit"):
            ports, attrs = interface(node.get("inherit"), ancestors + (name,))
        attrs.update(node.attrib)
        seen = set()
        for port in node:
            if port.tag not in ("input", "output"):
                continue
            key = (port.tag, port.get("name"))
            if not key[1] or key in seen:
                raise ValueError("duplicate or unnamed port in " + name)
            seen.add(key)
            previous = ports.get(key, {})
            if previous.get("type") and port.get("type") and previous["type"] != port.get("type"):
                raise ValueError(f"{name}.{key[1]}: incompatible inherited port type")
            merged = dict(previous)
            literal_attrs = ("value", "defaultgeomprop") if port.tag == "input" else ("value", "default")
            # A local default replaces an inherited spelling of that default.
            # An output's pass-through input and fallback literal are independent.
            if any(attr in port.attrib for attr in literal_attrs):
                for attr in literal_attrs:
                    merged.pop(attr, None)
            merged.update(port.attrib)
            ports[key] = merged
        return ports, attrs

    nodes = []
    for name in selected:
        ports, attrs = interface(name)
        # Infer legacy single outputs after all inherited attributes are overlaid.
        if not any(key[0] == "output" for key in ports):
            if attrs.get("type") == "multioutput":
                raise ValueError("multioutput NodeDef has no outputs")
            ports[("output", "out")] = {"type": attrs.get("type", "")}
        nodes.append((name, [
            (direction, port_name, port)
            for (direction, port_name), port in sorted(ports.items())
        ]))
    return nodes


if __name__ == "__main__":
    try:
        json.dump(extract(Path(sys.argv[1]), sys.argv[2:]), sys.stdout)
    except (OSError, ValueError, ET.ParseError) as error:
        print(str(error), file=sys.stderr)
        sys.exit(1)
