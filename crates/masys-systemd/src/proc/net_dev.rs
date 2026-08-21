//! `/proc/net/dev` - per-interface counters, and `/sys/class/net` for
//! the two facts that file does not carry.
//!
//! Read every sample: the file is 0.14 ms to read on the development
//! host, against a 99 ms sample, so it costs nothing to keep current.

use masys_domain::sample::Interface;

/// The columns, by position.
///
/// `/proc/net/dev`'s header is two lines and is not a table: `Receive`
/// and `Transmit` sit above eight fields each, so the only way to read it
/// is to count. After the name, fields 0..8 are receive and 8..16 are
/// transmit, in the same order both times: bytes, packets, errs, drop,
/// fifo, frame/colls, compressed, multicast/carrier.
const RX_BYTES: usize = 0;
const RX_PACKETS: usize = 1;
const RX_ERRS: usize = 2;
const RX_DROP: usize = 3;
const TX_BYTES: usize = 8;
const TX_PACKETS: usize = 9;
const TX_ERRS: usize = 10;
const TX_DROP: usize = 11;

/// Every interface's counters.
///
/// `up` and `loopback` are left false here - this file knows neither, and
/// `read_interfaces` fills them from `/sys/class/net`.
pub fn parse_net_dev(text: &str) -> Vec<Interface> {
    text.lines()
        .filter_map(|line| {
            // The name is everything before the colon, which is padded
            // away from it for short names and flush against it for long
            // ones. Splitting on the colon handles both without caring.
            let (name, counters) = line.split_once(':')?;
            let name = name.trim();
            // The header's first line has no colon; its second does, in
            // ` face |bytes ...`, which this rejects by way of the name
            // containing a space.
            if name.is_empty() || name.contains(char::is_whitespace) {
                return None;
            }
            let fields: Vec<u64> = counters.split_whitespace().filter_map(|f| f.parse().ok()).collect();
            if fields.len() < TX_DROP + 1 {
                return None;
            }
            Some(Interface {
                name: name.to_string(),
                rx_bytes: fields[RX_BYTES],
                rx_packets: fields[RX_PACKETS],
                rx_errs: fields[RX_ERRS],
                rx_drop: fields[RX_DROP],
                tx_bytes: fields[TX_BYTES],
                tx_packets: fields[TX_PACKETS],
                tx_errs: fields[TX_ERRS],
                tx_drop: fields[TX_DROP],
                up: false,
                loopback: false,
            })
        })
        .collect()
}

/// `ARPHRD_LOOPBACK`, from `/sys/class/net/<name>/type`.
const ARPHRD_LOOPBACK: &str = "772";

/// Every interface, with the two facts `/proc/net/dev` does not carry.
///
/// `operstate` rather than `carrier`: only `down` means down. A tun
/// device reports `unknown` while working - `tailscale0` on this host
/// does, and carries real traffic - so anything that is not `down` is
/// treated as usable. Reading it for every interface measured 0.61 ms.
pub fn read_interfaces() -> Vec<Interface> {
    let Ok(text) = std::fs::read_to_string("/proc/net/dev") else {
        return Vec::new();
    };
    parse_net_dev(&text)
        .into_iter()
        .map(|interface| {
            let attr =
                |name: &str| std::fs::read_to_string(format!("/sys/class/net/{}/{name}", interface.name)).map(|s| s.trim().to_string());
            Interface {
                up: attr("operstate").map(|state| state != "down").unwrap_or(true),
                loopback: attr("type").map(|kind| kind == ARPHRD_LOOPBACK).unwrap_or(false),
                ..interface
            }
        })
        .collect()
}
