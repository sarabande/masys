//! The mutations masys performs on processes: signals, nice, and IO
//! priority.
//!
//! Raw syscalls rather than shelling out to `kill(1)`/`renice(1)`: these
//! take a pid and an integer, there is nothing to parse, and a spawn per
//! action would be slower and would lose the errno that says *why* it was
//! refused.

use masys_domain::error::MasysError;
use masys_domain::service::{IoNiceClass, Signal};

/// The signal numbers, named rather than taken from `libc::SIGTERM` so
/// the mapping from masys's own `Signal` is visible in one place.
pub fn signal_number(signal: Signal) -> i32 {
    match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
        Signal::Hup => libc::SIGHUP,
        Signal::Int => libc::SIGINT,
        Signal::Usr1 => libc::SIGUSR1,
        Signal::Usr2 => libc::SIGUSR2,
    }
}

/// Nice is -20..=19, and the kernel silently clamps out-of-range values
/// rather than refusing them - which would make a typo look like it
/// worked. Clamping here means the value masys reports is the value that
/// was set.
pub fn clamp_nice(value: i32) -> i32 {
    value.clamp(-20, 19)
}

/// `ioprio_set`'s packed argument: the class in the top three bits, the
/// level in the low thirteen.
///
/// `Idle` has no levels - the kernel ignores the field entirely - so it
/// is passed as zero rather than as whatever the caller happened to
/// supply, which keeps the value masys sends equal to the value it means.
pub fn ioprio_value(class: IoNiceClass, level: i32) -> i32 {
    const SHIFT: i32 = 13;
    let (class_number, level) = match class {
        IoNiceClass::RealTime => (1, level.clamp(0, 7)),
        IoNiceClass::BestEffort => (2, level.clamp(0, 7)),
        IoNiceClass::Idle => (3, 0),
    };
    (class_number << SHIFT) | level
}

fn last_error(what: &str, pid: u32) -> MasysError {
    let e = std::io::Error::last_os_error();
    // errno is what says *why*, and the two that matter read very
    // differently to an operator: ESRCH means the process finished on its
    // own, which is usually fine, while EPERM means it is still there and
    // masys is not allowed to touch it.
    MasysError::Command(match e.raw_os_error() {
        Some(libc::ESRCH) => format!("{what}: process {pid} no longer exists"),
        Some(libc::EPERM) => format!("{what}: not permitted for process {pid} - it belongs to another user"),
        _ => format!("{what}: process {pid}: {e}"),
    })
}

pub fn kill(pid: u32, signal: Signal) -> Result<(), MasysError> {
    // SAFETY: `kill` takes two integers and touches no memory. A pid that
    // does not exist is reported through errno, not undefined behaviour.
    //
    // The pid is cast rather than validated because the kernel rejects
    // anything it does not own - but note that `kill(0, sig)` signals the
    // whole process group, so a zero pid is refused here rather than
    // passed through.
    if pid == 0 {
        return Err(MasysError::Command("kill: refusing to signal the entire process group".to_string()));
    }
    if unsafe { libc::kill(pid as libc::pid_t, signal_number(signal)) } != 0 {
        return Err(last_error("kill", pid));
    }
    Ok(())
}

pub fn renice(pid: u32, value: i32) -> Result<(), MasysError> {
    // `setpriority` returns -1 both on error and legitimately when the
    // new priority is -1, so errno must be cleared first to tell them
    // apart. This is the documented way to call it.
    unsafe { *libc::__errno_location() = 0 };
    // SAFETY: three integers, no memory touched.
    let result = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid, clamp_nice(value)) };
    if result == -1 && std::io::Error::last_os_error().raw_os_error() != Some(0) {
        return Err(last_error("renice", pid));
    }
    Ok(())
}

pub fn ionice(pid: u32, class: IoNiceClass, level: i32) -> Result<(), MasysError> {
    const IOPRIO_WHO_PROCESS: libc::c_int = 1;
    // SAFETY: `ioprio_set` has no libc wrapper, so it goes through
    // `syscall` directly. Three integer arguments, no memory touched.
    let result = unsafe { libc::syscall(libc::SYS_ioprio_set, IOPRIO_WHO_PROCESS, pid as libc::c_int, ioprio_value(class, level)) };
    if result != 0 {
        return Err(last_error("ionice", pid));
    }
    Ok(())
}

/// Runs a `systemctl` verb against one unit.
///
/// Through `systemctl` rather than D-Bus `StartUnit`, which is the
/// design's split: systemctl triggers polkit's authentication agent, so
/// an unprivileged masys can be granted the action, where a direct bus
/// call is simply refused. The cost is a spawn per action, which against
/// a human pressing a key is free.
///
/// stderr is carried through verbatim. polkit's refusal text is the whole
/// value of the error - "Access denied" versus "Interactive
/// authentication required" tell an operator different things to do next.
pub fn systemctl(verb: &str, unit: &str) -> Result<(), MasysError> {
    let output = std::process::Command::new("systemctl")
        .args([verb, unit])
        // No stdin: systemctl must never decide to prompt into a frame
        // masys is drawing.
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(MasysError::Io)?;
    if output.status.success() {
        return Ok(());
    }
    let message = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(MasysError::Command(if message.is_empty() { format!("systemctl {verb} {unit} failed") } else { message }))
}

/// Runs a `systemctl` verb that takes over the terminal.
///
/// `status()` with stdio inherited, where [`systemctl`] uses `output()`
/// with it captured. The difference is the whole point: `systemctl edit`
/// spawns `$EDITOR`, which needs a real terminal to draw on and a real
/// stdin to read. Capturing either gives an editor talking to a pipe,
/// which typically hangs.
///
/// The caller is responsible for having released the terminal first -
/// there is nothing this function can check to make sure of that.
///
/// stderr goes to the terminal rather than into the error, for the same
/// reason: it is already on screen by the time this returns.
pub fn systemctl_interactive(verb: &str, unit: &str) -> Result<(), MasysError> {
    let status = std::process::Command::new("systemctl")
        .args([verb, unit])
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit())
        .status()
        .map_err(MasysError::Io)?;
    if status.success() {
        return Ok(());
    }
    Err(MasysError::Command(format!("systemctl {verb} {unit} exited {}", status.code().unwrap_or(-1))))
}
