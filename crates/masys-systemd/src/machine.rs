//! What the host *is*: distro, kernel, architecture, CPU.
//!
//! Four files, none of which change while masys runs, so
//! `SystemdService` reads them once at connect time and clones the result
//! onto every snapshot.

use masys_domain::sample::Machine;

/// `/etc/os-release`'s `PRETTY_NAME`, unquoted.
///
/// The file is shell-ish `KEY=value` with optional quoting, and the value
/// legitimately contains spaces and parentheses (`NixOS 26.11 (Zokor)`),
/// so it is taken whole rather than tokenised. Falls back to `ID` and then
/// to a plain marker, because a host with no `PRETTY_NAME` is unusual but
/// not broken, and refusing to name it would lose the kernel line too.
pub fn parse_os_release(text: &str) -> String {
    let field = |key: &str| -> Option<String> {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix('='))
            .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_string())
            .filter(|value| !value.is_empty())
    };
    field("PRETTY_NAME").or_else(|| field("NAME")).or_else(|| field("ID")).unwrap_or_else(|| "unknown".to_string())
}

/// `(model, logical cpu count)` from `/proc/cpuinfo`.
///
/// The count is of `processor:` lines, which is logical CPUs -
/// hyperthreads included. That is deliberately the number a CPU
/// percentage is measured against, so `800%` on this 8-thread host means
/// "everything", and a single process legitimately exceeding 100% reads
/// correctly.
///
/// `model name` is absent on some architectures (aarch64 reports
/// `Processor` or nothing at all), so an empty model is a normal answer
/// rather than a parse failure.
pub fn parse_cpuinfo(text: &str) -> (String, u32) {
    let model = text
        .lines()
        .find_map(|line| line.strip_prefix("model name")?.split_once(':').map(|(_, v)| v.trim().to_string()))
        .unwrap_or_default();
    let cores = text.lines().filter(|line| line.starts_with("processor")).count() as u32;
    (model, cores)
}

/// Reads every file this needs. Missing files degrade to empty strings
/// rather than failing: a container with no `/etc/os-release` still has a
/// kernel worth naming, and this is a header label, not a measurement.
pub fn read_machine() -> Machine {
    let os_release = std::fs::read_to_string("/etc/os-release").unwrap_or_default();
    let cpuinfo = std::fs::read_to_string("/proc/cpuinfo").unwrap_or_default();
    let (cpu_model, cpu_cores) = parse_cpuinfo(&cpuinfo);
    Machine {
        distro: parse_os_release(&os_release),
        kernel: std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default().trim().to_string(),
        arch: arch(),
        cpu_model,
        cpu_cores,
    }
}

/// The machine hardware name, as `uname -m` reports it.
fn arch() -> String {
    // SAFETY: `utsname` is zeroed and correctly sized, and `uname` fills
    // it in. A negative return means it wrote nothing, which is why the
    // buffer is read only on success.
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut buf) } != 0 {
        return String::new();
    }
    let bytes: Vec<u8> = buf.machine.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}
