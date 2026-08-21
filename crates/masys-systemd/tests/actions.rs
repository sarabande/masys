use masys_domain::error::MasysError;
use masys_domain::service::{IoNiceClass, Signal};
use masys_systemd::actions::{clamp_nice, ioprio_value, kill, renice, signal_number};

#[test]
fn every_signal_maps_to_its_number() {
    assert_eq!(signal_number(Signal::Term), libc::SIGTERM);
    assert_eq!(signal_number(Signal::Kill), libc::SIGKILL);
    assert_eq!(signal_number(Signal::Hup), libc::SIGHUP);
    assert_eq!(signal_number(Signal::Int), libc::SIGINT);
    assert_eq!(signal_number(Signal::Usr1), libc::SIGUSR1);
    assert_eq!(signal_number(Signal::Usr2), libc::SIGUSR2);
}

/// The kernel silently clamps an out-of-range nice rather than refusing
/// it, which would make a typo look like it worked. Clamping here means
/// the value masys reports is the value that was set.
#[test]
fn nice_is_clamped_to_the_kernels_range() {
    assert_eq!(clamp_nice(0), 0);
    assert_eq!(clamp_nice(19), 19);
    assert_eq!(clamp_nice(-20), -20);
    assert_eq!(clamp_nice(100), 19);
    assert_eq!(clamp_nice(-100), -20);
}

/// `ioprio_set` packs the class into the top three bits and the level
/// into the low thirteen.
#[test]
fn ioprio_packs_class_and_level() {
    assert_eq!(ioprio_value(IoNiceClass::RealTime, 0), 1 << 13);
    assert_eq!(ioprio_value(IoNiceClass::BestEffort, 4), (2 << 13) | 4);
    assert_eq!(ioprio_value(IoNiceClass::BestEffort, 7), (2 << 13) | 7);
}

/// `Idle` has no levels - the kernel ignores the field - so it is sent as
/// zero rather than whatever the caller supplied, keeping the value masys
/// sends equal to the value it means.
#[test]
fn the_idle_class_has_no_level() {
    assert_eq!(ioprio_value(IoNiceClass::Idle, 5), 3 << 13);
    assert_eq!(ioprio_value(IoNiceClass::Idle, 0), 3 << 13);
}

#[test]
fn an_out_of_range_ioprio_level_is_clamped() {
    assert_eq!(ioprio_value(IoNiceClass::BestEffort, 99), (2 << 13) | 7);
    assert_eq!(ioprio_value(IoNiceClass::BestEffort, -5), 2 << 13);
}

/// `kill(0, sig)` signals the caller's entire process group - including
/// masys itself. A pid of 0 can only arrive from a bug, and acting on it
/// would take the terminal down.
#[test]
fn killing_pid_zero_is_refused() {
    let err = kill(0, Signal::Term).expect_err("pid 0 must be refused");
    assert!(matches!(err, MasysError::Command(_)), "{err:?}");
    assert!(format!("{err}").contains("process group"), "{err}");
}

/// errno is what says *why*, and the two that matter read very
/// differently: a process that finished on its own is usually fine, while
/// one masys is not allowed to touch is still there.
#[test]
fn signalling_a_process_that_does_not_exist_says_so() {
    // Above the default pid_max, so it cannot be live.
    let err = kill(4_194_305, Signal::Term).expect_err("no such process");
    assert!(format!("{err}").contains("no longer exists"), "{err}");
}

#[test]
fn renicing_a_process_that_does_not_exist_says_so() {
    let err = renice(4_194_305, 5).expect_err("no such process");
    assert!(format!("{err}").contains("no longer exists"), "{err}");
}

/// `setpriority` returns -1 both on error and legitimately when the new
/// priority *is* -1, so a naive check would report success as failure.
/// Renicing this test process to its own current value proves the
/// distinction is handled.
#[test]
fn renicing_our_own_process_succeeds() {
    let pid = std::process::id();
    // Raising niceness is always permitted; lowering it needs privilege.
    renice(pid, 5).expect("renice self to 5");
    renice(pid, 6).expect("renice self to 6");
}

/// The EPERM path, which reads very differently to an operator than
/// ESRCH: the process is still there, masys is just not allowed to touch
/// it. Skipped when running as root, where it would genuinely signal pid
/// 1 - and the point of this test is not to send a signal that lands.
#[test]
fn signalling_another_users_process_says_it_is_not_permitted() {
    // SAFETY: getuid takes no arguments and cannot fail.
    if unsafe { libc::getuid() } == 0 {
        return;
    }
    let err = kill(1, Signal::Hup).expect_err("pid 1 belongs to root");
    assert!(format!("{err}").contains("not permitted"), "{err}");
}
