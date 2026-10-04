// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Adapted from OpenUSD Hello World / Generic Prims / Inspecting Properties.
//! <https://openusd.org/release/tut_helloworld.html>
//! <https://openusd.org/release/tut_helloworld_redux.html>
//! <https://openusd.org/release/tut_inspect_and_author_props.html>
mod support;
use layerstack::Time;
fn main() {
    let directory = support::directory();
    let mut document = support::hello(&directory);
    let world = document.store_mut().path("/hello/world");
    let prim = document
        .stage()
        .stage()
        .prim(world, document.store())
        .unwrap();
    assert_eq!(
        document.store().tokens.resolve(prim.type_name().unwrap()),
        "Sphere",
        "generic prim retains the authored type"
    );
    let radius = prim.attribute("radius").unwrap();
    assert_eq!(
        radius.get(Time::Default).unwrap().value,
        layerstack::Value::Double(2.0),
        "authored sphere radius"
    );
    println!("{}", directory.join("HelloWorld.usda").display());
    println!(
        "Sphere properties: {:?}",
        prim.property_names()
            .iter()
            .map(|name| document.store().tokens.resolve(*name))
            .collect::<Vec<_>>()
    );
}
