// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

// Loads a USD file through Apple's SceneKit (which reads USD with ModelIO,
// as AR Quick Look does) and reports what it will draw.
//
// Usage: swift scenekit_check.swift SCENE.usdz OUT.png
//
// Prints `nodes N`, `geometries N` (nodes that carry geometry, i.e. meshes
// SceneKit draws, one per placement) and `bounds MIN MAX`, then renders the
// scene with default lighting from a camera that frames its bounds and
// writes OUT.png. Apple's stack ignores `UsdGeomPointInstancer`, so a file
// that relies on one reports only its prototypes here.
//
// `tests/instanced_references.rs` runs it when `LAYERSTACK_SCENEKIT=1`.

import AppKit
import SceneKit

let arguments = CommandLine.arguments
guard arguments.count == 3 else {
    FileHandle.standardError.write("usage: scenekit_check.swift SCENE.usdz OUT.png\n".data(using: .utf8)!)
    exit(2)
}
let scene: SCNScene
do {
    scene = try SCNScene(url: URL(fileURLWithPath: arguments[1]), options: nil)
} catch {
    FileHandle.standardError.write("cannot load \(arguments[1]): \(error)\n".data(using: .utf8)!)
    exit(1)
}

var nodes = 0
var geometries = 0
scene.rootNode.enumerateHierarchy { node, _ in
    nodes += 1
    if node.geometry != nil {
        geometries += 1
    }
}
print("nodes \(nodes)")
print("geometries \(geometries)")
let (low, high) = scene.rootNode.boundingBox
print("bounds (\(low.x), \(low.y), \(low.z)) (\(high.x), \(high.y), \(high.z))")

guard let device = MTLCreateSystemDefaultDevice() else {
    FileHandle.standardError.write("no Metal device to render with\n".data(using: .utf8)!)
    exit(1)
}
let renderer = SCNRenderer(device: device, options: nil)
renderer.scene = scene
renderer.autoenablesDefaultLighting = true
let center = SCNVector3((low.x + high.x) / 2, (low.y + high.y) / 2, (low.z + high.z) / 2)
let size = max(high.x - low.x, high.y - low.y, high.z - low.z)
let camera = SCNNode()
camera.camera = SCNCamera()
camera.camera!.zFar = Double(size) * 10
camera.position = SCNVector3(
    center.x + size * 0.7, center.y + size * 0.6, center.z + size * 0.9)
camera.look(at: center)
scene.rootNode.addChildNode(camera)
renderer.pointOfView = camera
let image = renderer.snapshot(
    atTime: 0, with: CGSize(width: 1200, height: 800), antialiasingMode: .multisampling4X)
guard
    let tiff = image.tiffRepresentation,
    let bitmap = NSBitmapImageRep(data: tiff),
    let png = bitmap.representation(using: .png, properties: [:])
else {
    FileHandle.standardError.write("cannot encode the render\n".data(using: .utf8)!)
    exit(1)
}
do {
    try png.write(to: URL(fileURLWithPath: arguments[2]))
} catch {
    FileHandle.standardError.write("cannot write \(arguments[2]): \(error)\n".data(using: .utf8)!)
    exit(1)
}
