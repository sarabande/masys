use masys_domain::journal::Priority;
use masys_systemd::journal::{parse_entry, parse_stream};

/// Captured from `journalctl -n1 -o json` on this machine, trimmed to the
/// fields masys reads plus a few it must ignore.
const LINE: &str = r#"{"_SYSTEMD_UNIT":"sshd.service","_HOSTNAME":"devbox","__REALTIME_TIMESTAMP":"1779332941369216","PRIORITY":"3","SYSLOG_IDENTIFIER":"sshd","_TRANSPORT":"stdout","MESSAGE":"too many authentication failures","_PID":"4821"}"#;

#[test]
fn an_entry_carries_the_four_fields_the_journal_buffer_shows() {
    let entry = parse_entry(LINE).expect("a parsed entry");
    assert_eq!(entry.unit.as_deref(), Some("sshd.service"));
    assert_eq!(entry.priority, Priority::Error);
    assert_eq!(entry.message, "too many authentication failures");
    // Journal timestamps are microseconds; Entry is milliseconds.
    assert_eq!(entry.timestamp_ms, 1_779_332_941_369);
}

/// Kernel records have no owning unit - that absence is what
/// `SectionKind::Kernel` is keyed on, so it must survive as `None`.
#[test]
fn a_kernel_record_has_no_unit() {
    let line = r#"{"__REALTIME_TIMESTAMP":"1779332941369216","PRIORITY":"4","_TRANSPORT":"kernel","MESSAGE":"i915 rcs0 GPU hang"}"#;
    let entry = parse_entry(line).expect("a parsed entry");
    assert_eq!(entry.unit, None);
    assert_eq!(entry.priority, Priority::Warning);
}

#[test]
fn every_priority_digit_maps_to_its_level() {
    for (digit, expected) in [
        ("0", Priority::Emergency),
        ("1", Priority::Alert),
        ("2", Priority::Critical),
        ("3", Priority::Error),
        ("4", Priority::Warning),
        ("5", Priority::Notice),
        ("6", Priority::Info),
        ("7", Priority::Debug),
    ] {
        let line = format!(r#"{{"PRIORITY":"{digit}","MESSAGE":"x","__REALTIME_TIMESTAMP":"1000"}}"#);
        assert_eq!(parse_entry(&line).expect("parsed").priority, expected, "priority {digit}");
    }
}

#[test]
fn a_record_with_no_priority_field_is_info() {
    let line = r#"{"MESSAGE":"x","__REALTIME_TIMESTAMP":"1000"}"#;
    assert_eq!(parse_entry(line).expect("parsed").priority, Priority::Info);
}

/// journalctl emits MESSAGE as an array of byte values when the message
/// is not valid UTF-8, which really happens for kernel records.
#[test]
fn a_byte_array_message_is_decoded_rather_than_shown_as_numbers() {
    let line = r#"{"MESSAGE":[104,101,108,108,111],"__REALTIME_TIMESTAMP":"1000","PRIORITY":"6"}"#;
    assert_eq!(parse_entry(line).expect("parsed").message, "hello");
}

#[test]
fn a_malformed_line_costs_one_entry_not_the_whole_query() {
    let stream = format!("{LINE}\nnot json at all\n{LINE}\n");
    assert_eq!(parse_stream(&stream).len(), 2, "the two good records survive the bad one between them");
}

#[test]
fn a_trailing_newline_does_not_produce_an_empty_entry() {
    assert_eq!(parse_stream(&format!("{LINE}\n")).len(), 1);
}

/// The `UNIT` field carries the unit systemd is reporting on; the
/// timestamp is microseconds and becomes milliseconds.
#[test]
fn unit_events_are_read_from_the_unit_field_and_timestamp() {
    let line = r#"{"MESSAGE_ID":"be02cf6855d2428ba40df7e9d022f03d","UNIT":"nightly-backup.service","_SYSTEMD_UNIT":"init.scope","__REALTIME_TIMESTAMP":"1779332941369216","MESSAGE":"Failed to start Nightly backup."}"#;
    assert_eq!(masys_systemd::journal::parse_unit_events(line), vec![("nightly-backup.service".to_string(), 1_779_332_941_369)]);
}

/// `_SYSTEMD_UNIT` is the *sender's* unit - init.scope for these - so a
/// record without the `UNIT` field names no unit and must be skipped
/// rather than attributed to systemd itself.
#[test]
fn a_record_without_a_unit_field_is_skipped() {
    let line = r#"{"MESSAGE_ID":"39f53479d3a045ac8e11786248231fbf","_SYSTEMD_UNIT":"init.scope","__REALTIME_TIMESTAMP":"1000000"}"#;
    assert!(masys_systemd::journal::parse_unit_events(line).is_empty());
}

#[test]
fn a_malformed_line_costs_one_record_not_the_backfill() {
    let good = r#"{"MESSAGE_ID":"x","UNIT":"a.service","__REALTIME_TIMESTAMP":"2000000"}"#;
    let stream = format!("{good}\nnot json\n\n{good}\n");
    assert_eq!(masys_systemd::journal::parse_unit_events(&stream).len(), 2);
}
