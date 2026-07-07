//! Foundation smoke tests: the wiring seam builds and events flow.
//!
//! These assert the architecture composes — not any transfer behaviour.

use peerbeam_engine::{DomainEvent, EngineBuilder, EngineConfig};

/// The engine builds from default config with no providers registered.
#[test]
fn builds_with_defaults() {
    let engine = EngineBuilder::with_defaults()
        .build()
        .expect("engine should build from defaults");

    assert_eq!(engine.registry().discovery().len(), 0);
    assert_eq!(engine.registry().transfer().len(), 0);
    assert!(engine.registry().encryption().is_none());
    assert!(!engine.config().device.name.is_empty());
}

/// A custom config is preserved through the builder.
#[test]
fn honours_custom_config() {
    let mut config = EngineConfig::default();
    config.device.name = "test-device".to_string();
    config.transfer.chunk_size = 4096;

    let engine = EngineBuilder::new(config).build().expect("engine builds");

    assert_eq!(engine.config().device.name, "test-device");
    assert_eq!(engine.config().transfer.chunk_size, 4096);
}

/// Events published after a subscription are delivered to that subscriber.
#[test]
fn event_stream_delivers() {
    let engine = EngineBuilder::with_defaults().build().unwrap();

    // No subscribers yet: publish reaches nobody.
    assert_eq!(engine.publish(DomainEvent::Error("noone".into())), 0);

    let mut rx = engine.subscribe();
    let delivered = engine.publish(DomainEvent::Error("boom".into()));
    assert_eq!(delivered, 1);

    match rx.try_recv() {
        Ok(DomainEvent::Error(msg)) => assert_eq!(msg, "boom"),
        other => panic!("expected Error event, got {other:?}"),
    }
}
