# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Record atomic namespace editing results with OpenUSD's UsdNamespaceEditor.

Run with a Python environment providing pxr (tested with OpenUSD 26.08).
Writes small reproducible USDA scenes and JSON expectations beside the harness.
"""
from pathlib import Path
import json
from pxr import Sdf, Usd

OUT = Path(__file__).resolve().parent.parent / 'fixtures' / 'namespace_edits'


def scene():
    stage = Usd.Stage.CreateInMemory()
    a = stage.DefinePrim('/A')
    stage.DefinePrim('/A/Child')
    stage.DefinePrim('/B')
    watch = stage.DefinePrim('/Watch')
    a.CreateAttribute('outputs:value', Sdf.ValueTypeNames.Double).Set(4.0)
    a.CreateAttribute('relative', Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression('Child//'))
    a.SetPropertyOrder(['relative', 'outputs:value'])
    watch.CreateRelationship('look').SetTargets(['/A', '/A/Child', '/A.outputs:value'])
    watch.CreateAttribute('inputs:value', Sdf.ValueTypeNames.Double).SetConnections(['/A.outputs:value'])
    watch.CreateAttribute('expression', Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression('/A// /AX// /A.outputs:value %/A:relative'))
    stage.GetRootLayer().defaultPrim = 'A'
    return stage


def snapshot(stage):
    return {
        'prims': sorted(str(p.GetPath()) for p in stage.Traverse()),
        'defaultPrim': stage.GetRootLayer().defaultPrim,
        'watchTargets': [str(p) for p in stage.GetRelationshipAtPath('/Watch.look').GetTargets()],
        'connections': [str(p) for p in stage.GetAttributeAtPath('/Watch.inputs:value').GetConnections()],
        'expression': stage.GetAttributeAtPath('/Watch.expression').Get().GetText(),
        'properties': {
            str(p.GetPath()): {
                'names': sorted(str(x.GetName()) for x in p.GetProperties()),
                'order': [str(n) for n in p.GetPropertyOrder()],
            }
            for p in stage.Traverse()
        },
        'relativeExpressions': {
            str(attr.GetPath()): attr.Get().GetText()
            for p in stage.Traverse() for attr in p.GetAttributes()
            if str(attr.GetTypeName()) == 'pathExpression' and attr.GetName() == 'relative'
        },
    }


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    result = {}
    for name, method, source, destination in [
        ('prim_reparent', 'MovePrimAtPath', '/A', '/B/New'),
        ('prim_rename', 'MovePrimAtPath', '/A', '/Renamed'),
        ('property_rename', 'MovePropertyAtPath', '/A.outputs:value', '/A.outputs:renamed'),
        ('property_reparent', 'MovePropertyAtPath', '/A.relative', '/B.relative'),
    ]:
        stage = scene()
        stage.GetRootLayer().Export(str(OUT / (name + '.usda')))
        editor = Usd.NamespaceEditor(stage)
        assert getattr(editor, method)(source, destination)
        can_apply = editor.CanApplyEdits()
        assert can_apply, can_apply.errors
        assert editor.ApplyEdits()
        result[name] = {'source': source, 'destination': destination, 'snapshot': snapshot(stage)}
    (OUT / 'oracle.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')


if __name__ == '__main__':
    main()
