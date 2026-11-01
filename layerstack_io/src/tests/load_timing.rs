// Copyright 2026 the LayerStack Authors
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! A load reports each layer it read, with the bytes it decoded and the
//! prim specs it made, and each composition after a batch of reads; with
//! a clock the host supplies, when each ran ([`LoadReport::reads`],
//! [`LoadReport::compositions`]).

use super::*;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;

/// A clock that moves one millisecond each time it is read, so the
/// readings follow the order the load reads it in.
#[derive(Debug, Default)]
struct Ticking(AtomicU64);

impl Clock for Ticking {
    fn now(&self) -> Duration {
        Duration::from_millis(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

const ROOT: &str = "#usda 1.0\ndef \"Rock\" (prepend references = @lamp.usda@</Lamp>) {}\n";
const LAMP: &str =
    "#usda 1.0\ndef \"Lamp\" (prepend references = @robot.usda@</Robot>) {\n def \"Bulb\" {}\n}\n";
const ROBOT: &str = "#usda 1.0\ndef \"Robot\" {}\n";

fn scene() -> Memory {
    Memory::with(&[
        ("root.usda", ROOT),
        ("lamp.usda", LAMP),
        ("robot.usda", ROBOT),
    ])
}

fn open(storage: Memory, clock: Option<LoadClock>) -> StageDocument<Memory> {
    StageDocument::open(
        storage,
        "root.usda",
        OpenOptions {
            loading: LoadOptions {
                clock,
                ..LoadOptions::default()
            },
            ..OpenOptions::default()
        },
    )
    .unwrap()
}

fn ms(millis: u64) -> Duration {
    Duration::from_millis(millis)
}

#[test]
fn without_a_clock_a_load_counts_its_work_and_times_nothing() {
    let doc = open(scene(), None);
    let report = doc.load_report();
    let reads: Vec<_> = report
        .reads
        .iter()
        .map(|read| {
            (
                doc.identifier(read.layer).unwrap(),
                read.bytes,
                read.prims,
                read.time,
            )
        })
        .collect();
    // In the order the reads began: the root reads its reference's
    // layer, which reads its own, before it ends.
    assert_eq!(
        reads,
        [
            ("root.usda", ROOT.len() as u64, 1, None),
            ("lamp.usda", LAMP.len() as u64, 2, None),
            ("robot.usda", ROBOT.len() as u64, 1, None),
        ]
    );
    assert_eq!(
        report.compositions,
        [Composition {
            layers: 3,
            time: None
        }]
    );
}

#[test]
fn a_clock_times_each_read_less_the_reads_nested_in_it() {
    let doc = open(scene(), Some(LoadClock::new(Ticking::default())));
    let report = doc.load_report();
    let times: Vec<_> = report.reads.iter().map(|read| read.time.unwrap()).collect();
    let span = |start, end| TimeSpan {
        start: ms(start),
        end: ms(end),
    };
    // root begins at 0, lamp at 1, robot at 2; robot ends at 3, lamp at
    // 4, root at 5.
    assert_eq!(
        times,
        [
            ReadTime {
                span: span(0, 5),
                own: ms(2)
            },
            ReadTime {
                span: span(1, 4),
                own: ms(2)
            },
            ReadTime {
                span: span(2, 3),
                own: ms(1)
            },
        ]
    );
    // The own times add up to the time spent reading.
    let own: Duration = times.iter().map(|time| time.own).sum();
    assert_eq!(own, times[0].span.duration());
    // Then the layers enter the store and the stage composes.
    assert_eq!(
        report.compositions,
        [Composition {
            layers: 3,
            time: Some(span(6, 7))
        }]
    );
}

/// A host's byte map: what it has not been given is unavailable.
#[derive(Debug, Default)]
struct Host(BTreeMap<String, Vec<u8>>);

impl Storage for Host {
    fn identify(&self, asset: &str, _: Option<&str>) -> Result<String, IoError> {
        Ok(asset.into())
    }
    fn read(&self, identifier: &str) -> Result<Vec<u8>, IoError> {
        self.0
            .get(identifier)
            .cloned()
            .ok_or_else(|| IoError::new(IoErrorKind::Unavailable, "not supplied"))
    }
    fn write(&mut self, identifier: &str, bytes: &[u8]) -> Result<(), IoError> {
        self.0.insert(identifier.into(), bytes.to_vec());
        Ok(())
    }
}

#[test]
fn loading_what_a_host_supplied_reports_each_read_and_the_composition_after() {
    let mut host = Host::default();
    host.write("root.usda", ROOT.as_bytes()).unwrap();
    let clock = LoadClock::new(Ticking::default());
    let mut doc = StageDocument::open(
        host,
        "root.usda",
        OpenOptions {
            loading: LoadOptions {
                clock: Some(clock.clone()),
                ..LoadOptions::default()
            },
            ..OpenOptions::default()
        },
    )
    .unwrap();
    assert_eq!(doc.load_options().clock, Some(clock));
    // The root alone: its reference is unavailable, so nothing nests.
    // Asking for it again found it still unavailable and composed with
    // nothing added.
    let opened = doc.load_report();
    assert_eq!(opened.reads.len(), 1);
    let added: Vec<_> = opened.compositions.iter().map(|c| c.layers).collect();
    assert_eq!(added, [1, 0]);
    let after_open = opened.compositions[1].time.unwrap().end;

    // Both supplied at once: the lamp reads the robot as it goes.
    doc.storage_mut()
        .write("lamp.usda", LAMP.as_bytes())
        .unwrap();
    doc.storage_mut()
        .write("robot.usda", ROBOT.as_bytes())
        .unwrap();
    let report = doc.load_requested_assets().unwrap();
    let reads: Vec<_> = report
        .reads
        .iter()
        .map(|read| (doc.identifier(read.layer).unwrap(), read.prims))
        .collect();
    assert_eq!(reads, [("lamp.usda", 2), ("robot.usda", 1)]);
    let lamp = report.reads[0].time.unwrap();
    let robot = report.reads[1].time.unwrap();
    assert!(lamp.span.start > after_open);
    assert!(lamp.span.start < robot.span.start && robot.span.end < lamp.span.end);
    assert_eq!(lamp.own + robot.own, lamp.span.duration());
    let [composition] = report.compositions.as_slice() else {
        panic!("one composition: {:?}", report.compositions);
    };
    assert_eq!(composition.layers, 2);
    assert!(composition.time.unwrap().start >= lamp.span.end);
}

#[test]
fn a_read_that_fails_is_logged_with_no_prims() {
    let storage = Memory::with(&[
        ("root.usda", ROOT),
        // OpenUSD refuses a USDA layer opening with a byte order mark.
        ("lamp.usda", "\u{feff}#usda 1.0\ndef \"Lamp\" {}\n"),
    ]);
    let doc = StageDocument::open(
        storage,
        "root.usda",
        OpenOptions {
            loading: LoadOptions {
                import_policy: ImportPolicy::AllowRecovery,
                clock: Some(LoadClock::new(Ticking::default())),
                ..LoadOptions::default()
            },
            ..OpenOptions::default()
        },
    )
    .unwrap();
    let report = doc.load_report();
    let lamp = report
        .reads
        .iter()
        .find(|read| doc.identifier(read.layer) == Some("lamp.usda"))
        .unwrap();
    assert!(!report.layers.contains(&lamp.layer));
    assert_eq!(report.asset_failures.len(), 1);
    assert_eq!(lamp.prims, 0);
    assert!(lamp.time.is_some());
}
