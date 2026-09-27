# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Loads the schema plugins usd-core is built without, from OpenUSD's source.

Import this before `pxr`. `layerstack_schemas` generates `usdMtlx` from an
OpenUSD source checkout, since the usd-core wheel is built without MaterialX
and so without the plugin. This module makes usd-core load those schemas as
a codeless plugin: for each, a temporary directory holds the source
`generatedSchema.usda` and a `plugInfo.json` declaring the plugin's schema
types (from the source `plugInfo.json`, with no `LibraryPath`), and
`PXR_PLUGINPATH_NAME` names it.

The checkout is `LAYERSTACK_OPENUSD_SOURCE`, at the wheel's release (the
generator checks the same checkout's version).
"""
import json
import os
import shutil
import tempfile

# The domains layerstack_schemagen reads from the source (`Origin::Source`).
SOURCE_DOMAINS = ["usdMtlx"]


def source_dir():
    """The OpenUSD checkout."""
    source = os.environ.get("LAYERSTACK_OPENUSD_SOURCE")
    if not source:
        raise SystemExit(
            "set LAYERSTACK_OPENUSD_SOURCE to an OpenUSD checkout at the wheel's release")
    return source


def plug_info_path(plugin):
    """The source `plugInfo.json` of `plugin`."""
    return os.path.join(source_dir(), "pxr", "usd", plugin, "plugInfo.json")


def plug_info(path):
    """A `plugInfo.json`, without its `#` comment lines."""
    with open(path) as f:
        text = "\n".join(line for line in f.read().splitlines() if not line.lstrip().startswith("#"))
    return json.loads(text)


def _enable():
    root = tempfile.mkdtemp(prefix="layerstack-codeless-")
    paths = []
    for plugin in SOURCE_DOMAINS:
        directory = os.path.join(root, plugin)
        os.makedirs(directory)
        shutil.copy(
            os.path.join(source_dir(), "pxr", "usd", plugin, "generatedSchema.usda"), directory)
        types = {}
        for entry in plug_info(plug_info_path(plugin))["Plugins"]:
            for name, info in entry["Info"].get("Types", {}).items():
                if "schemaIdentifier" in info:
                    types[name] = info
        codeless = {
            "Plugins": [{
                "Info": {"Types": types},
                "Name": plugin,
                "ResourcePath": ".",
                "Root": ".",
                "Type": "resource",
            }]
        }
        with open(os.path.join(directory, "plugInfo.json"), "w") as f:
            json.dump(codeless, f, indent=4)
        paths.append(directory)
    existing = os.environ.get("PXR_PLUGINPATH_NAME")
    os.environ["PXR_PLUGINPATH_NAME"] = os.pathsep.join(paths + ([existing] if existing else []))


_enable()
