# Copyright 2026 the LayerStack Authors
# SPDX-License-Identifier: Apache-2.0 OR MIT
"""Generate native OpenUSD 26.08 coordinated namespace edit evidence.

UsdNamespaceEditor owns root-stack edits and receives explicit dependent stages.
Fixtures are captured before mutation; JSON records independently computed results.
Use --check for a read-only comparison against the tracked fixture tree.
"""
from pathlib import Path
import argparse
import json
import tempfile
from pxr import Sdf, Usd

OUT = Path(__file__).resolve().parent.parent / 'fixtures' / 'namespace_coordinated'


def snapshot(stage):
    result = {'prims': sorted(str(p.GetPath()) for p in stage.TraverseAll()),
              'values': {}, 'targets': {}, 'connections': {}, 'expressions': {}}
    for prim in stage.TraverseAll():
        for rel in prim.GetRelationships():
            result['targets'][str(rel.GetPath())] = [str(p) for p in rel.GetTargets()]
        for attr in prim.GetAttributes():
            path = str(attr.GetPath())
            connections = attr.GetConnections()
            if connections:
                result['connections'][path] = [str(p) for p in connections]
            value = attr.Get()
            if str(attr.GetTypeName()) == 'pathExpression' and value is not None:
                result['expressions'][path] = value.GetText()
            elif isinstance(value, (float, int)):
                result['values'][path] = value
    return result


def layer_snapshot(layer):
    paths = []
    layer.Traverse('/', lambda p: paths.append(str(p)) if isinstance(layer.GetObjectAtPath(p), Sdf.PrimSpec) else None)
    return {'specs': sorted(paths), 'relocates': [[str(a), str(b)] for a, b in layer.relocates]}


def make_stage(directory, name):
    path = directory / (name + '.usda')
    if path.exists():
        path.unlink()
    return Usd.Stage.CreateNew(str(path))


def size(prim, value):
    prim.CreateAttribute('size', Sdf.ValueTypeNames.Double).Set(value)


def record(directory, primary, dependents, source, destination, layers):
    for layer in layers:
        layer.Save()
    inventory = directory / 'inventory.usda'
    inventory.write_text('#usda 1.0\n( subLayers = [' + ', '.join('@' + Path(l.identifier).name + '@' for l in layers) + '] )\n')
    editor = Usd.NamespaceEditor(primary)
    for stage in dependents:
        editor.AddDependentStage(stage)
    if '.' in source:
        assert editor.MovePropertyAtPath(source, destination)
    else:
        assert editor.MovePrimAtPath(source, destination)
    allowed = editor.CanApplyEdits()
    assert allowed, allowed.errors
    assert editor.ApplyEdits()
    return {'root': Path(primary.GetRootLayer().identifier).name,
            'dependents': [Path(s.GetRootLayer().identifier).name for s in dependents],
            'source': source, 'destination': destination,
            'stages': {Path(s.GetRootLayer().identifier).name: snapshot(s) for s in [primary] + dependents},
            'layers': {Path(l.identifier).name: layer_snapshot(l) for l in layers}}


def split():
    directory = OUT / 'split'
    directory.mkdir(parents=True, exist_ok=True)
    weak = make_stage(directory, 'weak')
    size(weak.DefinePrim('/A'), 7.0)
    weak.DefinePrim('/A/Child')
    primary = make_stage(directory, 'root')
    primary.GetRootLayer().subLayerPaths = ['weak.usda']
    size(primary.DefinePrim('/A'), 2.0)
    primary.DefinePrim('/B')
    return record(directory, primary, [], '/A', '/B/New', [primary.GetRootLayer(), weak.GetRootLayer()])


def split_property():
    directory = OUT / 'split_property'
    directory.mkdir(parents=True, exist_ok=True)
    weak = make_stage(directory, 'weak')
    size(weak.DefinePrim('/A'), 7.0)
    primary = make_stage(directory, 'root')
    primary.GetRootLayer().subLayerPaths = ['weak.usda']
    size(primary.DefinePrim('/A'), 2.0)
    dep = make_stage(directory, 'dependent')
    dep.DefinePrim('/Alias').GetReferences().AddReference('root.usda', '/A')
    size(dep.OverridePrim('/Alias'), 3.0)
    return record(directory, primary, [dep], '/A.size', '/A.renamed',
                  [primary.GetRootLayer(), weak.GetRootLayer(), dep.GetRootLayer()])


def relocation(payload=False, reparent=False):
    directory = OUT / (('payload' if payload else 'reference') + ('_reparent' if reparent else ''))
    directory.mkdir(parents=True, exist_ok=True)
    asset = make_stage(directory, 'asset')
    asset.DefinePrim('/Asset')
    asset_child = asset.DefinePrim('/Asset/Child')
    size(asset_child, 1.0)
    asset_child.CreateAttribute('expression', Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression('/Asset/Child.size /Asset/Child/Leaf//'))
    asset.DefinePrim('/Asset/Child/Leaf')
    asset_watch = asset.DefinePrim('/Asset/Watch')
    asset_watch.CreateRelationship('look').SetTargets(['/Asset/Child'])
    asset_watch.CreateAttribute('inputs:value', Sdf.ValueTypeNames.Double).SetConnections(['/Asset/Child.size'])
    asset_watch.CreateAttribute('expression', Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression('/Asset/Child// /Asset/Child.size %/Asset/Child:expression'))
    primary = make_stage(directory, 'root')
    model = primary.DefinePrim('/Model')
    if payload:
        model.GetPayloads().AddPayload('asset.usda', '/Asset')
    else:
        model.GetReferences().AddReference('asset.usda', '/Asset')
    size(primary.OverridePrim('/Model/Child'), 2.0)
    if reparent:
        primary.DefinePrim('/Else/Group')
        return record(directory, primary, [], '/Model/Child', '/Else/Group/Renamed', [primary.GetRootLayer(), asset.GetRootLayer()])
    dep = make_stage(directory, 'dependent')
    dep.DefinePrim('/Use').GetReferences().AddReference('root.usda', '/Model')
    size(dep.OverridePrim('/Use/Child'), 3.0)
    watch = dep.DefinePrim('/Watch')
    watch.CreateRelationship('look').SetTargets(['/Use/Child'])
    watch.CreateAttribute('inputs:value', Sdf.ValueTypeNames.Double).SetConnections(['/Use/Child.size'])
    watch.CreateAttribute('expression', Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression('/Use/Child//'))
    direct = make_stage(directory, 'direct')
    direct.DefinePrim('/Direct').GetReferences().AddReference('root.usda', '/Model/Child')
    return record(directory, primary, [dep, direct], '/Model/Child', '/Model/Renamed',
                  [primary.GetRootLayer(), asset.GetRootLayer(), dep.GetRootLayer(), direct.GetRootLayer()])


def expression_mapping_cases():
    # These fixtures isolate PcpMapFunction invertibility, without relying on
    # UsdNamespaceEditor to repair authored expressions before composition.
    directory = OUT / 'expression_maps'
    directory.mkdir(parents=True, exist_ok=True)
    asset = make_stage(directory, 'asset')
    asset.DefinePrim('/Asset')
    asset_child = asset.DefinePrim('/Asset/Child')
    asset_watch = asset.DefinePrim('/Asset/Watch')
    suffixes = {'subtree': '//', 'property': '.size', 'reference': ':expression'}
    def expressions(prim, prefix, labels=('source', 'target')):
        for label, child in zip(labels, ['Child', 'Renamed']):
            for family, suffix in suffixes.items():
                text = prefix + '/' + child + suffix
                if family == 'reference':
                    text = '%' + text
                prim.CreateAttribute(label + ':' + family, Sdf.ValueTypeNames.PathExpression).Set(Sdf.PathExpression(text))
    for prim in [asset_child, asset_watch]:
        expressions(prim, '/Asset')
    asset.GetRootLayer().Save()
    root = make_stage(directory, 'root')
    root.DefinePrim('/Model').GetReferences().AddReference('asset.usda', '/Asset')
    root.DefinePrim('/Copy').GetReferences().AddInternalReference('/Model/Watch')
    root.DefinePrim('/Copy2').GetReferences().AddInternalReference('/Copy')
    local = root.DefinePrim('/Local')
    expressions(local, '/Model')
    root.DefinePrim('/Else').GetReferences().AddReference('asset.usda', '/Asset')
    class2 = root.CreateClassPrim('/Class2')
    expressions(class2, '/Else')
    class1 = root.CreateClassPrim('/Class1')
    class1.GetInherits().AddInherit('/Class2')
    root.DefinePrim('/ClassModel').GetInherits().AddInherit('/Class1')
    specific = root.CreateClassPrim('/Class')
    class_child = root.DefinePrim('/Class/Child')
    expressions(class_child, '/Else', labels=('outsideSource', 'outsideTarget'))
    expressions(specific, '/Class', labels=('classSource', 'classTarget'))
    # Both paths in the specific inherit map's destination domain are blocked.
    expressions(specific, '/Model2')
    root.DefinePrim('/Model2').GetInherits().AddInherit('/Class')
    class_special = root.CreateClassPrim('/ClassSpecial')
    class_special.GetSpecializes().AddSpecialize('/Class2')
    root.DefinePrim('/SpecialModel').GetSpecializes().AddSpecialize('/ClassSpecial')
    root.DefinePrim('/SpecialModel2').GetSpecializes().AddSpecialize('/Class')
    root.GetRootLayer().relocates = [(Sdf.Path('/Model/Child'), Sdf.Path('/Model/Renamed')),
                                    (Sdf.Path('/Else/Child'), Sdf.Path('/Else/Renamed')),
                                    (Sdf.Path('/Model2/Child'), Sdf.Path('/Model2/Renamed')),
                                    (Sdf.Path('/SpecialModel2/Child'), Sdf.Path('/SpecialModel2/Renamed'))]
    root.GetRootLayer().Save()
    properties = {}
    for path in ['/Model/Watch', '/Model/Renamed', '/Copy', '/Copy2', '/Local',
                 '/Class2', '/Class1', '/ClassModel', '/Class', '/Class/Child', '/Model2', '/Model2/Renamed',
                 '/ClassSpecial', '/SpecialModel', '/SpecialModel2', '/SpecialModel2/Renamed']:
        for attr in root.GetPrimAtPath(path).GetAttributes():
            properties[str(attr.GetPath())] = attr.Get().GetText()
    return properties


def generate():
    OUT.mkdir(parents=True, exist_ok=True)
    oracle = {'split': split(), 'split_property': split_property(), 'reference': relocation(), 'payload': relocation(True),
              'reference_reparent': relocation(False, True), 'payload_reparent': relocation(True, True)}
    (OUT / 'oracle.json').write_text(json.dumps(oracle, indent=2, sort_keys=True) + '\n')
    (OUT / 'expression_maps.json').write_text(json.dumps(expression_mapping_cases(), indent=2, sort_keys=True) + '\n')
    # Native USDA formatting includes trailing spaces and an extra blank line.
    # Keep these fixed fixtures free of whitespace-only diffs.
    for path in OUT.rglob('*.usda'):
        path.write_text('\n'.join(line.rstrip() for line in path.read_text().splitlines()).rstrip() + '\n')


def main():
    global OUT
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true', help='compare fresh native evidence without changing tracked files')
    args = parser.parse_args()
    if not args.check:
        generate()
        return
    expected = OUT
    with tempfile.TemporaryDirectory(prefix='layerstack-namespace-') as directory:
        OUT = Path(directory)
        generate()
        actual_files = {p.relative_to(OUT) for p in OUT.rglob('*') if p.is_file()}
        expected_files = {p.relative_to(expected) for p in expected.rglob('*') if p.is_file()}
        differences = sorted(str(p) for p in actual_files ^ expected_files)
        differences += sorted(str(p) for p in actual_files & expected_files
                              if (OUT / p).read_bytes() != (expected / p).read_bytes())
        if differences:
            raise SystemExit('Native namespace fixture drift: ' + ', '.join(differences))
    print('Native namespace fixtures match.')


if __name__ == '__main__':
    main()
