//! Offline, typo-detecting address codes. Not identities or credentials.
use std::net::{Ipv4Addr, SocketAddrV4};
const DIGITS: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";

pub fn public_ip(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_documentation()
        || a == 0
        || a >= 224
        || (a == 100 && (64..128).contains(&b))
        || (a == 198 && (b == 18 || b == 19))
        || (a == 192 && ((b == 0 && c == 0) || (b == 88 && c == 99))))
}
/// A directory-free 12-digit number for a public IPv4 at the standard port.
/// The leading zero is reserved (directory-issued desktop codes never start with zero).
/// Nonstandard mapped ports keep the existing VR address format.
pub fn internet_direct_code(addr: SocketAddrV4, standard_port: u16) -> Option<String> {
    if !public_ip(*addr.ip()) || addr.port() == 0 {
        return None;
    }
    if addr.port() != standard_port {
        return Some(encode(addr));
    }
    let digits = format!("0{:010}", u32::from(*addr.ip()));
    Some(format!("{digits}{}", check_digit(&digits)))
}
fn check_digit(s: &str) -> u8 {
    (10 - s
        .bytes()
        .rev()
        .enumerate()
        .map(|(i, b)| {
            let n = b - b'0';
            if i % 2 == 0 {
                let d = n * 2;
                d / 10 + d % 10
            } else {
                n
            }
        })
        .sum::<u8>()
        % 10)
        % 10
}
pub fn decode_internet_direct(code: &str, port: u16) -> Result<SocketAddrV4, &'static str> {
    if code.len() != 12
        || !code.starts_with('0')
        || !code.bytes().all(|b| b.is_ascii_digit())
        || check_digit(&code[..11]) != code.as_bytes()[11] - b'0'
    {
        return Err("公网联机码校验失败，请重新复制。");
    }
    let ip = Ipv4Addr::from(code[1..11].parse::<u32>().map_err(|_| "公网地址无效。")?);
    if !public_ip(ip) || port == 0 {
        return Err("联机码不包含有效公网地址。");
    }
    Ok(SocketAddrV4::new(ip, port))
}
fn usable(addr: SocketAddrV4) -> bool {
    let a = addr.ip().octets()[0];
    addr.port() != 0 && a != 0 && a < 224 && !addr.ip().is_loopback()
}
pub fn encode(addr: SocketAddrV4) -> String {
    let value = (u64::from(u32::from(*addr.ip())) << 16) | u64::from(addr.port());
    let mut s = String::from("VR-");
    for i in (0..10).rev() {
        if i == 4 {
            s.push('-');
        }
        s.push(DIGITS[((value >> (i * 5)) & 31) as usize] as char);
    }
    s.push(DIGITS[(value % 31) as usize] as char);
    s
}
pub fn decode(code: &str) -> Result<SocketAddrV4, &'static str> {
    let compact: Vec<_> = code
        .bytes()
        .filter(|b| !b.is_ascii_whitespace() && *b != b'-')
        .map(|b| b.to_ascii_uppercase())
        .collect();
    if compact.len() != 13 || &compact[..2] != b"VR" {
        return Err("请输入对方新版桌面端显示的 VR 联机码，不是旧版 9 位编码或网址。");
    }
    let digit = |b| {
        DIGITS
            .iter()
            .position(|&d| d == b)
            .map(|d| d as u64)
            .ok_or("联机码含无效字符。")
    };
    let mut v = 0;
    for b in &compact[2..12] {
        v = (v << 5) | digit(*b)?;
    }
    if v >> 48 != 0 || v % 31 != digit(compact[12])? {
        return Err("联机码校验失败，请重新复制。");
    }
    let addr = SocketAddrV4::new(Ipv4Addr::from((v >> 16) as u32), v as u16);
    usable(addr)
        .then_some(addr)
        .ok_or("联机码包含无效网络地址。")
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn round_trip() {
        for s in ["192.168.7.15:34789", "8.8.8.8:443"] {
            let a = s.parse().unwrap();
            assert_eq!(decode(&encode(a).to_lowercase()), Ok(a));
        }
    }
    #[test]
    fn reject_invalid() {
        for s in [
            "VR-00000-000000",
            "851023857",
            "VR-XXXXX-XXXXXX",
            "ws://localhost",
        ] {
            assert!(decode(s).is_err());
        }
        let mut code = encode("10.20.0.1:34789".parse().unwrap());
        code.pop();
        code.push('Z');
        assert!(decode(&code).is_err());
    }
    #[test]
    fn numeric_wan_codes_do_not_need_a_directory() {
        let a = "8.8.4.4:34789".parse().unwrap();
        let n = internet_direct_code(a, 34789).unwrap();
        assert!(n.starts_with('0'));
        assert_eq!(decode_internet_direct(&n, 34789), Ok(a));
        let mut typo = n.into_bytes();
        typo[4] = if typo[4] == b'1' { b'2' } else { b'1' };
        assert!(decode_internet_direct(std::str::from_utf8(&typo).unwrap(), 34789).is_err());
        let mapped = "8.8.4.4:40000".parse().unwrap();
        assert_eq!(
            decode(&internet_direct_code(mapped, 34789).unwrap()),
            Ok(mapped)
        );
        assert!(internet_direct_code("192.168.1.18:34789".parse().unwrap(), 34789).is_none());
        assert!(decode_internet_direct("000000000000", 34789).is_err());
    }
    #[test]
    fn no_private_wan() {
        for s in [
            "100.64.0.1",
            "10.0.0.1",
            "192.0.2.1",
            "127.0.0.1",
            "224.0.0.1",
        ] {
            assert!(!public_ip(s.parse().unwrap()));
        }
    }
}
