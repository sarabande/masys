//! `/proc/[pid]/cgroup`, which has two entirely different formats.

/// The cgroup path a process belongs to.
///
/// Under the unified hierarchy (cgroup v2) the file is one line,
/// `0::/user.slice/…`, and there is nothing to choose between. Under v1 -
/// still the default on older Debian and RHEL, and common inside
/// containers - it is one line *per controller*:
///
/// ```text
/// 12:pids:/user.slice
/// 11:memory:/user.slice
/// 1:name=systemd:/user.slice/session-2.scope
/// 0::/user.slice/session-2.scope
/// ```
///
/// Taking the first line, as this used to, picks whichever controller the
/// kernel happened to list first - a path that is real but is not the one
/// systemd organises units by, so processes group under the wrong thing.
///
/// The order below is by authority: the unified hierarchy first, then
/// systemd's own named v1 hierarchy, then any remaining controller as a
/// last resort so a host with an unusual layout still groups by
/// *something* rather than not at all.
pub fn parse_cgroup(text: &str) -> Option<String> {
    let mut fallback = None;
    for line in text.lines() {
        // `hierarchy-ID:controller-list:path`, and the path may itself
        // contain colons, so split only twice from the left.
        let mut parts = line.splitn(3, ':');
        let (Some(_id), Some(controllers), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            continue;
        };
        if path.is_empty() {
            continue;
        }
        // The unified hierarchy has an empty controller list.
        if controllers.is_empty() {
            return Some(path.to_string());
        }
        if controllers.split(',').any(|c| c == "name=systemd") {
            return Some(path.to_string());
        }
        fallback.get_or_insert_with(|| path.to_string());
    }
    fallback
}
