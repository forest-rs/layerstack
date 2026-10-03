# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record all schema-generated C++ Sdr light nodes with their USD definition types."""
import argparse
import json
import pathlib
from pxr import Sdr, Sdf, Usd
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--check", action="store_true")
args = parser.parse_args()
registry = Sdr.Registry()
def value(v, usd_type):
    if v is None:
        return None
    if usd_type == "bool":
        return bool(v)
    if isinstance(v, Sdf.AssetPath):
        return v.path
    if isinstance(v, (str, float, int, bool)):
        return v
    return list(v)
nodes = []
for identifier in sorted(registry.GetShaderNodeIdentifiers()):
    node = registry.GetShaderNodeByIdentifier(identifier, ["USD"])
    if not node or node.GetSourceType() != "USD":
        continue
    ports = []
    for kind, names, get in [("input", node.GetShaderInputNames(), node.GetShaderInput), ("output", node.GetShaderOutputNames(), node.GetShaderOutput)]:
        for name in names:
            p = get(name)
            metadata = p.GetMetadata()
            usd_type = metadata["sdrUsdDefinitionType"]
            ports.append({"name": str(name), "kind": kind, "usd_type": usd_type,
                "default": value(p.GetDefaultValue(), usd_type),
                "asset_identifier": metadata.get("__SDR__isAssetIdentifier") == "1"})
    metadata = node.GetMetadata()
    nodes.append({"identifier": str(identifier), "source_type": str(node.GetSourceType()),
        "context": metadata["context"], "subdomain": metadata["subdomain"],
        "ports": sorted(ports, key=lambda p: p["kind"] + p["name"])})
result = {"version": ".".join(map(str, Usd.GetVersion()[1:])), "nodes": nodes}
output = pathlib.Path(__file__).resolve().parents[1] / "fixtures" / "lux_nodes.json"
if args.check:
    assert json.loads(output.read_text()) == result
else:
    output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
