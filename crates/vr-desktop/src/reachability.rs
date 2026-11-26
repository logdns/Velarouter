//! What the user may type into "对方的联机码".
//!
//! Three forms exist, and they must be unambiguous because a wrong guess costs
//! a failed connection:
//!
//! * `VR-…` — a LAN code: the code *is* the address, no lookup at all;
//! * `0…` (12 digits) — a public IPv4/port encoded directly;
//! * any other 12 digits — a rendezvous code: the broker path finds the peer.
use std::net::{Ipv6Addr, SocketAddr};
use vr_net::direct::decode_internet_direct;
use vr_proto::{is_internet_code, DEFAULT_AGENT_PORT};

#[derive(Debug, PartialEq)]
pub enum Destination {
    Direct(SocketAddr),
    /// 12-digit rendezvous code (public broker).
    Meet(String),
    /// 9-digit code assigned by a self-hosted signal server.
    Signal(String),
}

/// Ordinary global unicast only: omit documentation, transition and special-use
/// prefixes so a bogus IPv6 code cannot send traffic somewhere private.
pub fn global6(ip: Ipv6Addr) -> bool {
    let [a, b, ..] = ip.segments();
    a >> 13 == 1
        && a != 0x2002
        && !(a == 0x2001 && (b < 0x200 || b == 0xdb8))
        && !(a == 0x3fff && b < 0x1000)
}

fn code6(ip: Ipv6Addr) -> String {
    let value = u128::from(ip);
    format!("VR6-{value:032X}-{:02X}", value % 251)
}

fn parse6(s: &str) -> Result<Ipv6Addr, &'static str> {
    let bad = "IPv6 联机码无效，请重新复制。";
    let digits = s
        .get(4..36)
        .filter(|s| s.bytes().all(|b| b.is_ascii_hexdigit()))
        .ok_or(bad)?;
    let ip = Ipv6Addr::from(u128::from_str_radix(digits, 16).map_err(|_| bad)?);
    (global6(ip) && s.eq_ignore_ascii_case(&code6(ip)))
        .then_some(ip)
        .ok_or(bad)
}

pub fn parse(input: &str) -> Result<Destination, &'static str> {
    let code: String = input.split_whitespace().collect();
    if code.is_empty() {
        return Err("请先输入对方的联机码。");
    }
    if code
        .get(..4)
        .is_some_and(|s| s.eq_ignore_ascii_case("VR6-"))
    {
        return parse6(&code)
            .map(|ip| Destination::Direct(SocketAddr::new(ip.into(), DEFAULT_AGENT_PORT)));
    }
    if code.len() == 9 && code.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(Destination::Signal(code));
    }
    if is_internet_code(&code) {
        if code.starts_with('0') {
            // Legacy direct code: public IPv4 + port packed into 12 digits.
            return match decode_internet_direct(&code, DEFAULT_AGENT_PORT) {
                Ok(a) => Ok(Destination::Direct(a.into())),
                Err(_) => Err("公网直连码无效，请重新复制。"),
            };
        }
        return Ok(Destination::Meet(code));
    }
    // LAN code: `VR-XXXXXX` decodes to address + port.
    match vr_net::direct::decode(&code) {
        Ok(addr) => Ok(Destination::Direct(addr.into())),
        Err(_) => Err("联机码格式不对：应为 12 位数字，或 VR- 开头的局域网码。"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn forms_are_unambiguous() {
        assert_eq!(
            parse("1234 5678 9012"),
            Ok(Destination::Meet("123456789012".into()))
        );
        let ip = Ipv6Addr::from(0x2606_4700_4700_0000_0000_0000_0000_1111u128);
        let code = code6(ip);
        assert_eq!(
            parse(&code),
            Ok(Destination::Direct(SocketAddr::new(ip.into(), 34789)))
        );
        assert!(parse(&(code + "0")).is_err());
        for bad in ["2001:db8::1", "::1", "fd00::2", "fe80::1", "2002::1234"] {
            assert!(!global6(bad.parse().unwrap()));
        }
        assert!(parse("hello").is_err());
        assert!(parse("").is_err());
    }

    #[test]
    fn public_direct_code_roundtrips() {
        let v4 = std::net::SocketAddrV4::new(std::net::Ipv4Addr::new(8, 8, 4, 4), 34789);
        let code = vr_net::direct::internet_direct_code(v4, DEFAULT_AGENT_PORT).unwrap();
        assert_eq!(parse(&code), Ok(Destination::Direct(SocketAddr::V4(v4))));
    }
}
