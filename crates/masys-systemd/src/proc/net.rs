//! `/proc/net/{tcp,tcp6,udp,udp6,unix}` - the kernel's socket tables,
//! read to give a socket descriptor an address.
//!
//! `/proc/[pid]/fd/7` resolves to `socket:[38271]`, which names an inode
//! and says nothing else. Every socket the kernel holds appears in one of
//! these tables keyed by that inode, so one pass over them turns a number
//! into `tcp 0.0.0.0:5432 LISTEN` - the difference between a row that
//! identifies a service and one that does not.
//!
//! The tables are read whole and indexed once per detail open, not once
//! per descriptor: a process with four hundred sockets would otherwise
//! re-read and re-parse the same files four hundred times.

use std::collections::HashMap;
use std::net::{Ipv4Addr, Ipv6Addr};

use masys_domain::proc_detail::FdTarget;

/// Every socket the kernel knows, by inode.
pub type SocketTable = HashMap<u64, FdTarget>;

/// TCP states as `/proc/net/tcp` spells them - the `st` column, in hex.
///
/// Named rather than numbered because the number is meaningless on sight
/// and `LISTEN` is the whole point: it is what separates a server from a
/// client that happens to have a connection open.
fn tcp_state(code: &str) -> &'static str {
    match code {
        "01" => "ESTABLISHED",
        "02" => "SYN_SENT",
        "03" => "SYN_RECV",
        "04" => "FIN_WAIT1",
        "05" => "FIN_WAIT2",
        "06" => "TIME_WAIT",
        "07" => "CLOSE",
        "08" => "CLOSE_WAIT",
        "09" => "LAST_ACK",
        "0A" => "LISTEN",
        "0B" => "CLOSING",
        _ => "UNKNOWN",
    }
}

/// `0100007F:1F90` -> `127.0.0.1:8080`.
///
/// The address half is the raw bytes of the kernel's `in_addr` printed as
/// hex, which on a little-endian host means the octets arrive reversed -
/// hence `from_bits(u32::from_str_radix(..).swap_bytes())` rather than a
/// plain parse. Getting this backwards is silent: `0100007F` reads as a
/// perfectly plausible `1.0.0.127`.
fn parse_v4(field: &str) -> Option<String> {
    let (address, port) = field.split_once(':')?;
    let raw = u32::from_str_radix(address, 16).ok()?;
    let port = u16::from_str_radix(port, 16).ok()?;
    Some(format!("{}:{port}", Ipv4Addr::from_bits(raw.swap_bytes())))
}

/// The same, for `/proc/net/tcp6`'s 32 hex digits.
///
/// Byte-swapped per 32-bit word rather than across the whole address:
/// the kernel prints four `__be32` words, each in host order.
fn parse_v6(field: &str) -> Option<String> {
    let (address, port) = field.split_once(':')?;
    if address.len() != 32 {
        return None;
    }
    let port = u16::from_str_radix(port, 16).ok()?;
    let words: Option<Vec<u32>> = (0..4).map(|i| u32::from_str_radix(&address[i * 8..i * 8 + 8], 16).ok()).collect();
    let mut octets = [0u8; 16];
    for (index, word) in words?.into_iter().enumerate() {
        octets[index * 4..index * 4 + 4].copy_from_slice(&word.swap_bytes().to_be_bytes());
    }
    // Bracketed, because `::1:5432` is ambiguous and `[::1]:5432` is not.
    Some(format!("[{}]:{port}", Ipv6Addr::from(octets)))
}

/// One `tcp`/`tcp6`/`udp`/`udp6` table.
///
/// The header line and any malformed row are skipped rather than failing
/// the parse: these files are read live while the kernel writes them, and
/// losing one socket is a better outcome than losing the whole table.
fn parse_inet(text: &str, v6: bool, tcp: bool) -> SocketTable {
    let address = if v6 { parse_v6 } else { parse_v4 };
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // sl, local, rem, st, tx:rx, tr:when, retrnsmt, uid, timeout, inode
            let (local, remote, state, inode) = (fields.first()?, fields.get(2)?, fields.get(3)?, fields.get(9)?);
            let _ = local;
            let local = address(fields.get(1)?)?;
            let inode: u64 = inode.parse().ok()?;
            let target = if tcp {
                let state = tcp_state(state);
                // A listening socket's peer is always `0.0.0.0:0`, which
                // is not a peer. Reporting it as one would make every
                // server look like it had a connection to nowhere.
                let peer = address(remote).filter(|_| state != "LISTEN");
                FdTarget::Tcp { local, peer, state: state.to_string() }
            } else {
                FdTarget::Udp { local }
            };
            Some((inode, target))
        })
        .collect()
}

/// The `SO_ACCEPTCON` bit in `/proc/net/unix`'s `Flags` column - the
/// kernel's own mark for "this socket is listening".
///
/// Read from the flag rather than inferred from the socket having a path,
/// which is the mistake that looks right until you run it: a client that
/// *connected* to `/run/user/1000/bus` reports that same path, so
/// path-alone reported all 87 of dbus-broker's accepted connections as
/// listeners.
const SO_ACCEPTCON: u32 = 0x10000;

/// `/proc/net/unix`. The path is the last field and is absent for the
/// abstract and unnamed sockets that make up most of the table.
fn parse_unix(text: &str) -> SocketTable {
    text.lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // Num, RefCount, Protocol, Flags, Type, St, Inode, Path
            let inode: u64 = fields.get(6)?.parse().ok()?;
            let listening = u32::from_str_radix(fields.get(3)?, 16).is_ok_and(|flags| flags & SO_ACCEPTCON != 0);
            // An abstract socket's name begins with `@` and is not a path
            // on disk; it is still a name worth showing, so it is kept.
            let path = fields.get(7).map(|p| p.to_string());
            Some((inode, FdTarget::Unix { path, listening }))
        })
        .collect()
}

/// One `/proc/net` file and the parser that reads it: the name to look
/// for, and what to do with the text if it is there.
type Source = (&'static str, fn(&str) -> SocketTable);

/// Every socket table the host has, merged into one index.
///
/// Missing files are skipped, not errors: a kernel built without IPv6 has
/// no `tcp6`, and a container may have none of them.
pub fn socket_table(read: impl Fn(&str) -> Option<String>) -> SocketTable {
    let sources: [Source; 5] = [
        ("tcp", |text| parse_inet(text, false, true)),
        ("tcp6", |text| parse_inet(text, true, true)),
        ("udp", |text| parse_inet(text, false, false)),
        ("udp6", |text| parse_inet(text, true, false)),
        ("unix", parse_unix),
    ];
    sources.into_iter().filter_map(|(name, parse)| read(name).map(|text| parse(&text))).flatten().collect()
}

/// Reads the tables from the real `/proc/net`.
pub fn read_socket_table() -> SocketTable {
    socket_table(|name| std::fs::read_to_string(format!("/proc/net/{name}")).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_v4_address_is_byte_swapped() {
        // `0100007F` is 127.0.0.1 with its octets reversed, and reads as
        // a perfectly plausible 1.0.0.127 if you forget that.
        assert_eq!(parse_v4("0100007F:1F90").as_deref(), Some("127.0.0.1:8080"));
        assert_eq!(parse_v4("00000000:1538").as_deref(), Some("0.0.0.0:5432"));
    }

    #[test]
    fn a_v6_address_swaps_within_each_word() {
        assert_eq!(parse_v6("00000000000000000000000001000000:1538").as_deref(), Some("[::1]:5432"));
    }

    #[test]
    fn a_listening_socket_reports_no_peer() {
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n   \
                    0: 00000000:1538 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 38271 1 0000 100\n";
        let table = parse_inet(text, false, true);
        assert_eq!(
            table.get(&38271),
            Some(&FdTarget::Tcp { local: "0.0.0.0:5432".into(), peer: None, state: "LISTEN".into() }),
            "a listening socket's `0.0.0.0:0` peer is not a peer: {table:?}"
        );
    }

    #[test]
    fn an_established_socket_reports_its_peer() {
        let text = "  sl  local_address rem_address   st\n   \
                    1: 0100007F:1F90 0100007F:9C40 01 00000000:00000000 00:00000000 00000000  1000        0 44012 1\n";
        let table = parse_inet(text, false, true);
        assert_eq!(
            table.get(&44012),
            Some(&FdTarget::Tcp { local: "127.0.0.1:8080".into(), peer: Some("127.0.0.1:40000".into()), state: "ESTABLISHED".into() })
        );
    }

    /// The listening socket and a client connected to it report the
    /// *same* path. Only the `SO_ACCEPTCON` flag tells them apart, and
    /// getting it from the path instead reported all 87 of dbus-broker's
    /// accepted connections as listeners on a real host.
    #[test]
    fn only_the_accept_flag_marks_a_unix_socket_as_listening() {
        let text = "Num       RefCount Protocol Flags    Type St Inode Path\n\
                    ffff9c0: 00000002 00000000 00010000 0001 01 38999 /run/postgresql/.s.PGSQL.5432\n\
                    ffff9c2: 00000003 00000000 00000000 0001 03 39001 /run/postgresql/.s.PGSQL.5432\n\
                    ffff9c1: 00000002 00000000 00000000 0001 03 39000\n";
        let table = parse_unix(text);
        assert_eq!(table.get(&38999), Some(&FdTarget::Unix { path: Some("/run/postgresql/.s.PGSQL.5432".into()), listening: true }));
        assert_eq!(
            table.get(&39001),
            Some(&FdTarget::Unix { path: Some("/run/postgresql/.s.PGSQL.5432".into()), listening: false }),
            "a client of that socket names the same path and is not a listener"
        );
        assert_eq!(
            table.get(&39000),
            Some(&FdTarget::Unix { path: None, listening: false }),
            "an unnamed socket has no path, and that is not an error"
        );
    }

    /// These files are read while the kernel is writing them, so a torn
    /// line has to cost one socket rather than the whole table.
    #[test]
    fn a_malformed_row_is_skipped_not_fatal() {
        let text = "  sl  local_address\n   0: garbage\n   1: 00000000:1538 00000000:0000 0A a b c d 1000 0 38271 1\n";
        assert_eq!(parse_inet(text, false, true).len(), 1);
    }

    #[test]
    fn a_missing_table_is_not_an_error() {
        // A kernel with no IPv6 has no tcp6 file at all.
        let table = socket_table(|name| (name == "unix").then(|| "Num RefCount\nx: 1 0 10000 1 1 7 /run/x\n".to_string()));
        assert_eq!(table.len(), 1);
    }
}
