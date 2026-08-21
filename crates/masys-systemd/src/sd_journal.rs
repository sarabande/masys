//! Reading the journal in process, through `libsystemd`.
//!
//! Exists for one reason: **following**. A subprocess cannot follow
//! without a long-lived child and a reader thread, and masys's event loop
//! is single-threaded with no channels by intent. Everything else here -
//! a bounded tail query - the `journalctl -o json` path already does
//! perfectly well, which is why that path remains and is not a fallback
//! in the apologetic sense: it is what runs wherever this does not.
//!
//! **`dlopen`, never linked.** The signatures below are declared by hand,
//! so the build needs no systemd headers, no `pkg-config`, and nothing
//! distro-specific; a masys built this way has zero `libsystemd` entries
//! in `ldd` and still reads the journal. If the library is absent or
//! stripped, `load` returns `None` and the subprocess path takes over
//! with nothing else changing.

use std::ffi::{CString, c_char, c_int, c_void};
use std::sync::OnceLock;

use masys_domain::journal::{Entry, Priority};

/// `sd_journal`, which is opaque and only ever held as a pointer.
type SdJournal = c_void;

/// Open the local journal only. No remote logs, no `/var/log/journal`
/// from another host - masys is a local-host tool and says so.
const SD_JOURNAL_LOCAL_ONLY: c_int = 1;

/// `sd_journal_wait` returning "nothing happened".
const SD_JOURNAL_NOP: c_int = 0;

/// The entry points, resolved once.
///
/// Nine, not the ten the design listed: `sd_journal_next` is not needed
/// because `tail` walks *backwards* from the end, which is both simpler
/// and able to stop early.
struct Lib {
    open: unsafe extern "C" fn(*mut *mut SdJournal, c_int) -> c_int,
    close: unsafe extern "C" fn(*mut SdJournal),
    add_match: unsafe extern "C" fn(*mut SdJournal, *const c_void, usize) -> c_int,
    flush_matches: unsafe extern "C" fn(*mut SdJournal),
    seek_tail: unsafe extern "C" fn(*mut SdJournal) -> c_int,
    previous_skip: unsafe extern "C" fn(*mut SdJournal, u64) -> c_int,
    get_data: unsafe extern "C" fn(*mut SdJournal, *const c_char, *mut *const c_void, *mut usize) -> c_int,
    get_realtime_usec: unsafe extern "C" fn(*mut SdJournal, *mut u64) -> c_int,
    wait: unsafe extern "C" fn(*mut SdJournal, u64) -> c_int,
}

// SAFETY: `Lib` is nine function pointers into a library that is never
// unloaded. Nothing in it is interior-mutable and nothing points at
// per-thread state. The `sd_journal` *handle* is a different matter and
// is deliberately not shared - see `Reader`.
unsafe impl Send for Lib {}
unsafe impl Sync for Lib {}

/// One symbol, or `None` if the library does not have it.
///
/// A partial load is treated as no load at all: a library exporting eight
/// of the nine is not a version to work around, it is a surprise, and a
/// surprise should take the fallback rather than half a feature.
unsafe fn symbol<T>(handle: *mut c_void, name: &str) -> Option<T> {
    let cname = CString::new(name).ok()?;
    // SAFETY: `handle` came from `dlopen` and has not been closed, and
    // `cname` is a valid NUL-terminated string that outlives the call.
    let address = unsafe { libc::dlsym(handle, cname.as_ptr()) };
    if address.is_null() {
        return None;
    }
    // SAFETY: the caller names `T` as the signature declared in
    // `sd-journal.h` for this symbol. This is the one place the hand-copied
    // declarations above are taken on trust, which is why they are all in
    // one screen and each is used exactly once.
    Some(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&address) })
}

/// Where to look for `libsystemd`, in order.
///
/// 1. `$MASYS_LIBSYSTEMD` - a universal escape hatch rather than distro
///    knowledge in the core.
/// 2. The soname, which resolves on Debian, Ubuntu, Fedora, RHEL, Arch
///    and SUSE through the normal loader path.
///
/// No path list. Soname-only `dlopen` *does* fail on NixOS, which keeps
/// no global library path and no `ldconfig` cache - the development host
/// is one - and the escape hatch is what covers that until NixOS gets a
/// view of its own.
fn candidates() -> Vec<CString> {
    std::env::var("MASYS_LIBSYSTEMD")
        .ok()
        .into_iter()
        .chain(std::iter::once("libsystemd.so.0".to_string()))
        .filter_map(|name| CString::new(name).ok())
        .collect()
}

fn load() -> Option<Lib> {
    for name in candidates() {
        // SAFETY: `name` is NUL-terminated and outlives the call.
        let handle = unsafe { libc::dlopen(name.as_ptr(), libc::RTLD_LAZY) };
        if handle.is_null() {
            continue;
        }
        // SAFETY: each `symbol` call names the signature `sd-journal.h`
        // declares for that symbol; see the `Lib` fields above.
        let lib = unsafe {
            Some(Lib {
                open: symbol(handle, "sd_journal_open")?,
                close: symbol(handle, "sd_journal_close")?,
                add_match: symbol(handle, "sd_journal_add_match")?,
                flush_matches: symbol(handle, "sd_journal_flush_matches")?,
                seek_tail: symbol(handle, "sd_journal_seek_tail")?,
                previous_skip: symbol(handle, "sd_journal_previous_skip")?,
                get_data: symbol(handle, "sd_journal_get_data")?,
                get_realtime_usec: symbol(handle, "sd_journal_get_realtime_usec")?,
                wait: symbol(handle, "sd_journal_wait")?,
            })
        };
        if lib.is_some() {
            // The handle is deliberately never `dlclose`d: the symbols
            // live as long as the process, and closing a library whose
            // function pointers are still held is how a tidy shutdown
            // becomes a segfault.
            return lib;
        }
    }
    None
}

/// The library, loaded at most once.
///
/// `None` means the subprocess path is what masys will use, on this host,
/// for the rest of the run.
pub fn available() -> bool {
    lib().is_some()
}

fn lib() -> Option<&'static Lib> {
    static LIB: OnceLock<Option<Lib>> = OnceLock::new();
    LIB.get_or_init(load).as_ref()
}

/// An open cursor over one unit's journal.
///
/// Held across ticks so that following costs a single non-blocking
/// `sd_journal_wait` rather than re-opening the journal four times a
/// second.
pub struct Reader {
    lib: &'static Lib,
    journal: *mut SdJournal,
    unit: String,
}

impl Reader {
    /// Opens the journal, filtered to one unit.
    ///
    /// The match is `_SYSTEMD_UNIT=`, a trusted field stamped by journald
    /// from the sender's cgroup and unforgeable by the sender - the same
    /// argument the `journalctl -u` path already makes, carried over
    /// unchanged.
    pub fn open(unit: &str) -> Option<Reader> {
        let lib = lib()?;
        let mut journal: *mut SdJournal = std::ptr::null_mut();
        // SAFETY: `journal` is a valid out-pointer for the duration.
        if unsafe { (lib.open)(&mut journal, SD_JOURNAL_LOCAL_ONLY) } < 0 || journal.is_null() {
            return None;
        }
        let reader = Reader { lib, journal, unit: unit.to_string() };
        let match_text = format!("_SYSTEMD_UNIT={unit}");
        // SAFETY: the handle is open, and `match_text` outlives the call.
        // A zero length asks libsystemd to measure the NUL-terminated
        // string itself, which is why the bytes must not contain one.
        let added = unsafe { (lib.add_match)(reader.journal, match_text.as_ptr() as *const c_void, match_text.len()) };
        if added < 0 {
            return None;
        }
        Some(reader)
    }

    pub fn unit(&self) -> &str {
        &self.unit
    }

    /// One field of the entry under the cursor.
    ///
    /// The bytes libsystemd hands back are its own and are valid only
    /// until the cursor moves, so this copies before returning - the
    /// single most important rule in this file.
    fn field(&self, name: &str) -> Option<String> {
        let cname = CString::new(name).ok()?;
        let mut data: *const c_void = std::ptr::null();
        let mut len: usize = 0;
        // SAFETY: the handle is open and positioned on an entry; `data`
        // and `len` are valid out-pointers.
        if unsafe { (self.lib.get_data)(self.journal, cname.as_ptr(), &mut data, &mut len) } < 0 || data.is_null() {
            return None;
        }
        // SAFETY: libsystemd guarantees `data` points at `len` readable
        // bytes until the cursor next moves, and nothing moves it here.
        let bytes = unsafe { std::slice::from_raw_parts(data as *const u8, len) };
        // The field arrives as `NAME=value`, and only the value is wanted.
        let text = String::from_utf8_lossy(bytes);
        text.split_once('=').map(|(_, value)| value.to_string())
    }

    fn entry_here(&self) -> Option<Entry> {
        let mut usec: u64 = 0;
        // SAFETY: the handle is open and positioned on an entry.
        if unsafe { (self.lib.get_realtime_usec)(self.journal, &mut usec) } < 0 {
            return None;
        }
        Some(Entry {
            timestamp_ms: usec / 1000,
            unit: self.field("_SYSTEMD_UNIT"),
            priority: priority_of(self.field("PRIORITY").as_deref()),
            message: self.field("MESSAGE").unwrap_or_default(),
        })
    }

    /// The last `lines` entries for this unit, oldest first.
    ///
    /// Walks backwards from the tail rather than forwards from a seek,
    /// because `sd_journal_seek_tail` positions *after* the last entry -
    /// stepping back is what lands on one.
    pub fn tail(&self, lines: usize) -> Vec<Entry> {
        // SAFETY: the handle is open. `flush_matches` then re-adding
        // would be wrong here - the match is this reader's identity - so
        // only the position is reset.
        unsafe { (self.lib.seek_tail)(self.journal) };
        let mut entries = Vec::with_capacity(lines.min(256));
        while entries.len() < lines {
            // SAFETY: the handle is open; a non-positive return means
            // there is nothing further back and the loop stops.
            if unsafe { (self.lib.previous_skip)(self.journal, 1) } <= 0 {
                break;
            }
            match self.entry_here() {
                Some(entry) => entries.push(entry),
                None => break,
            }
        }
        entries.reverse();
        entries
    }

    /// Whether anything has arrived since the last look. Never blocks.
    ///
    /// A zero timeout makes this a *check*: `sd_journal_wait(j, 0)`
    /// returns `NOP` in microseconds when the journal has not moved,
    /// which is what makes four wakes a second affordable.
    pub fn changed(&self) -> bool {
        // SAFETY: the handle is open, and a zero timeout cannot block.
        unsafe { (self.lib.wait)(self.journal, 0) != SD_JOURNAL_NOP }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        // SAFETY: the handle was opened by `open`, has not been closed,
        // and is not used again after this.
        unsafe {
            (self.lib.flush_matches)(self.journal);
            (self.lib.close)(self.journal);
        }
    }
}

/// The same mapping the `-o json` parser uses, against the same field.
fn priority_of(raw: Option<&str>) -> Priority {
    match raw {
        Some("0") => Priority::Emergency,
        Some("1") => Priority::Alert,
        Some("2") => Priority::Critical,
        Some("3") => Priority::Error,
        Some("4") => Priority::Warning,
        Some("5") => Priority::Notice,
        Some("7") => Priority::Debug,
        _ => Priority::Info,
    }
}
