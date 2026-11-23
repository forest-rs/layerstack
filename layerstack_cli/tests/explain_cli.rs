// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Property explanations through the installed command's interface.

#![cfg(not(target_family = "wasm"))]

use std::{
    fs,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[test]
fn trace_authored_sources_on_the_composed_stage() {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir =
        std::env::temp_dir().join(format!("layerstack-explain-{}-{nonce}", std::process::id()));
    fs::create_dir(&dir).unwrap();
    fs::write(
        dir.join("base.usda"),
        r#"#usda 1.0
def "Model"
{
    custom float weight = 1
    custom float animated.timeSamples = { 0: 2, 10: 4 }
    custom float blocked = 12
    custom token status = "ready"
    custom rel parts = [</Model/A>, </Model/B>]
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("scene.usda"),
        r#"#usda 1.0
(
    subLayers = [@base.usda@]
)
over "Model"
{
    custom float weight = 2
    float weight.connect = </Model.animated>
    custom float blocked = None
    delete rel parts = </Model/A>
}
"#,
    )
    .unwrap();
    fs::write(
        dir.join("reference.usda"),
        "#usda 1.0\ndef \"Assembly\" (references = @base.usda@</Model>) {}\n",
    )
    .unwrap();
    let run = |args: &[&str], status| {
        let o = Command::new(env!("CARGO_BIN_EXE_layerstack"))
            .arg("explain")
            .args(args)
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(status), "{o:?}");
        String::from_utf8(o.stdout).unwrap()
    };
    let weight = run(&["scene.usda", "/Model.weight"], 0);
    assert!(weight.contains("value (default): 2"), "{weight}");
    assert!(weight.find("contributed").unwrap() < weight.find("shadowed").unwrap());
    assert!(weight.contains("base.usda") && weight.contains("scene.usda"));
    assert!(weight.contains("connections (listOp): [/Model.animated]"));
    let animated = run(
        &["--json", "--time", "5", "scene.usda", "/Model.animated"],
        0,
    );
    assert!(
        animated.contains(r#""source":"timeSamples","preview":"3""#),
        "{animated}"
    );
    let rel = run(&["--json", "scene.usda", "/Model.parts"], 0);
    assert!(rel.contains(r#""paths":["/Model/B"]"#), "{rel}");
    assert!(rel.contains(r#""edits":{"delete":["/Model/A"]}"#), "{rel}");
    assert!(rel.contains(r#""edits":{"explicit":["/Model/A","/Model/B"]}"#));
    let mapped = run(&["reference.usda", "/Assembly.parts"], 0);
    assert!(mapped.contains("base.usda  /Model.parts"), "{mapped}");
    assert!(mapped.contains("/Model/A maps to /Assembly/A"));
    assert!(run(&["scene.usda", "/Model.blocked"], 0).contains("blocked  "));
    assert!(run(&["scene.usda", "/Model.status"], 0).contains("value (default): ready"));
    assert!(run(&["--json", "scene.usda", "/Model.absent"], 2).contains("missing_property"));
    assert!(run(&["--json", "scene.usda", "/Model"], 2).contains("invalid_path"));
    assert!(
        run(
            &["--json", "--time", "NaN", "scene.usda", "/Model.animated"],
            2
        )
        .contains("invalid_time")
    );
    fs::write(
        dir.join("clips.usda"),
        r#"#usda 1.0
def "Shot" (clips = { dictionary default = {
 asset[] assetPaths = [@base.usda@]
 string primPath = "/Model"
 double2[] active = [(0,0)]
 asset manifestAssetPath = @manifest.usda@
 }}) { float animated; }
"#,
    )
    .unwrap();
    fs::write(
        dir.join("manifest.usda"),
        r#"#usda 1.0
def "Model" {
 float animated
}
"#,
    )
    .unwrap();
    let clips = run(&["--time", "5", "clips.usda", "/Shot.animated"], 0);
    assert!(clips.contains("value (valueClips): 3"), "{clips}");
    assert!(clips.contains("base.usda  /Model.animated"));
    fs::remove_file(dir.join("base.usda")).unwrap();
    let incomplete = run(&["--json", "scene.usda", "/Model.weight"], 2);
    assert!(
        incomplete.contains("incomplete_stage") && incomplete.contains("base.usda"),
        "{incomplete}"
    );
    fs::remove_dir_all(dir).unwrap();
}
