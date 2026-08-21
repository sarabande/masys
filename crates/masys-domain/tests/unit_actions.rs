mod fake;

use fake::{Call, FakePlatformService, FakeSystemService};
use masys_domain::platform::PlatformId;
use masys_domain::service::{IoNiceClass, PlatformService, Signal, SystemService};
use masys_domain::unit_actions;

#[test]
fn start_passes_the_unit_name_through() {
    let service = FakeSystemService::default();
    unit_actions::start(&service, "restic-backup.service").expect("start");
    assert_eq!(service.only_call(), Call::Start("restic-backup.service".to_string()));
}

#[test]
fn stop_passes_the_unit_name_through() {
    let service = FakeSystemService::default();
    unit_actions::stop(&service, "otel-collector.service").expect("stop");
    assert_eq!(service.only_call(), Call::Stop("otel-collector.service".to_string()));
}

#[test]
fn restart_passes_the_unit_name_through() {
    let service = FakeSystemService::default();
    unit_actions::restart(&service, "restic-backup.service").expect("restart");
    assert_eq!(service.only_call(), Call::Restart("restic-backup.service".to_string()));
}

#[test]
fn reload_passes_the_unit_name_through() {
    let service = FakeSystemService::default();
    unit_actions::reload(&service, "nginx.service").expect("reload");
    assert_eq!(service.only_call(), Call::Reload("nginx.service".to_string()));
}

#[test]
fn kill_passes_pid_and_signal_through() {
    let service = FakeSystemService::default();
    unit_actions::kill(&service, 1204, Signal::Term).expect("kill");
    assert_eq!(service.only_call(), Call::Kill { pid: 1204, signal: Signal::Term });
}

#[test]
fn renice_passes_pid_and_value_through() {
    let service = FakeSystemService::default();
    unit_actions::renice(&service, 1204, -5).expect("renice");
    assert_eq!(service.only_call(), Call::Renice { pid: 1204, value: -5 });
}

#[test]
fn ionice_passes_pid_class_and_level_through() {
    let service = FakeSystemService::default();
    unit_actions::ionice(&service, 1204, IoNiceClass::BestEffort, 4).expect("ionice");
    assert_eq!(service.only_call(), Call::Ionice { pid: 1204, class: IoNiceClass::BestEffort, level: 4 });
}

/// Both ports are object-safe and usable behind a `Box`, which is how the
/// composition root will hand real adapters to `App` in a later plan.
#[test]
fn both_ports_work_through_a_trait_object() {
    let system: Box<dyn SystemService> = Box::new(FakeSystemService::default());
    unit_actions::start(system.as_ref(), "x.service").expect("start through a trait object");

    let platform: Box<dyn PlatformService> = Box::new(FakePlatformService);
    assert_eq!(platform.id(), PlatformId::Unsupported);
}
