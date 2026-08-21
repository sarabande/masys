use masys_systemd::machine::{parse_cpuinfo, parse_os_release};

/// Captured from this machine. The value contains spaces *and*
/// parentheses, which is why it is taken whole rather than tokenised.
const OS_RELEASE: &str = "ID=nixos\nPRETTY_NAME=\"NixOS 26.11 (Zokor)\"\nVERSION_ID=\"26.11\"\n";

#[test]
fn pretty_name_is_taken_whole_and_unquoted() {
    assert_eq!(parse_os_release(OS_RELEASE), "NixOS 26.11 (Zokor)");
}

#[test]
fn a_single_quoted_value_is_unquoted_too() {
    assert_eq!(parse_os_release("PRETTY_NAME='Debian GNU/Linux 12 (bookworm)'\n"), "Debian GNU/Linux 12 (bookworm)");
}

/// A host with no PRETTY_NAME is unusual but not broken, and refusing to
/// name it would cost the whole machine line.
#[test]
fn a_missing_pretty_name_falls_back_rather_than_failing() {
    assert_eq!(parse_os_release("NAME=\"Alpine Linux\"\nID=alpine\n"), "Alpine Linux");
    assert_eq!(parse_os_release("ID=alpine\n"), "alpine");
    assert_eq!(parse_os_release(""), "unknown");
}

/// An empty value must not win over a later usable one.
#[test]
fn an_empty_pretty_name_falls_through() {
    assert_eq!(parse_os_release("PRETTY_NAME=\"\"\nID=alpine\n"), "alpine");
}

#[test]
fn cpuinfo_reports_the_model_and_counts_logical_cpus() {
    let text = "processor\t: 0\nmodel name\t: Intel(R) Core(TM) i7-6700 CPU @ 3.40GHz\nsiblings\t: 8\n\n\
                processor\t: 1\nmodel name\t: Intel(R) Core(TM) i7-6700 CPU @ 3.40GHz\n";
    let (model, cores) = parse_cpuinfo(text);
    assert_eq!(model, "Intel(R) Core(TM) i7-6700 CPU @ 3.40GHz");
    assert_eq!(cores, 2, "one per processor line - hyperthreads included");
}

/// aarch64 and some VMs report no `model name` at all. That is a normal
/// answer, not a parse failure, and the core count still matters.
#[test]
fn cpuinfo_without_a_model_name_still_counts_cpus() {
    let (model, cores) = parse_cpuinfo("processor\t: 0\nBogoMIPS\t: 50.00\n\nprocessor\t: 1\nBogoMIPS\t: 50.00\n");
    assert_eq!(model, "");
    assert_eq!(cores, 2);
}
