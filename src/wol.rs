//! Wake-on-LAN magic packets. Tests send to a local UDP socket, never the LAN broadcast.

use std::net::UdpSocket;

pub fn parse_mac(raw: &str) -> Result<[u8; 6], &'static str> {
    let hex: String = raw
        .chars()
        .filter(|c| c.is_ascii_hexdigit())
        .collect::<String>()
        .to_ascii_lowercase();
    if hex.len() != 12 {
        return Err("MAC must be 6 bytes");
    }
    let mut mac = [0u8; 6];
    for i in 0..6 {
        mac[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| "MAC must be 6 bytes")?;
    }
    if mac.iter().all(|b| *b == 0) {
        return Err("MAC must be 6 bytes");
    }
    Ok(mac)
}

pub fn format_mac(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut pkt = [0xffu8; 102];
    for i in 0..16 {
        let off = 6 + i * 6;
        pkt[off..off + 6].copy_from_slice(&mac);
    }
    pkt
}

pub fn send_magic_to(mac: [u8; 6], dest: &str) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.send_to(&magic_packet(mac), dest)?;
    Ok(())
}

pub fn send_wol(mac: [u8; 6]) -> std::io::Result<()> {
    send_magic_to(mac, "255.255.255.255:9")
}

pub fn list_macs() -> Vec<String> {
    #[cfg(target_os = "macos")]
    {
        macos::list_macs()
    }
    #[cfg(not(target_os = "macos"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use super::format_mac;
    use std::ffi::CStr;
    use std::os::raw::{c_char, c_int, c_void};

    const AF_LINK: u8 = 18;
    const IFF_UP: u32 = 0x1;
    const IFF_LOOPBACK: u32 = 0x8;

    #[repr(C)]
    struct IfAddrs {
        next: *mut IfAddrs,
        name: *const c_char,
        flags: u32,
        addr: *const SockAddr,
        _netmask: *const SockAddr,
        _dst: *const SockAddr,
        _data: *mut c_void,
    }

    #[repr(C)]
    struct SockAddr {
        len: u8,
        family: u8,
        _data: [u8; 14],
    }

    #[repr(C)]
    struct SockAddrDl {
        len: u8,
        family: u8,
        _index: u16,
        _ty: u8,
        nlen: u8,
        alen: u8,
        _slen: u8,
        data: [u8; 12],
    }

    extern "C" {
        fn getifaddrs(ifap: *mut *mut IfAddrs) -> c_int;
        fn freeifaddrs(ifa: *mut IfAddrs);
    }

    fn keep_iface(name: &str) -> bool {
        name.starts_with("en") || name.starts_with("eth") || name.starts_with("wlan")
    }

    pub fn list_macs() -> Vec<String> {
        let mut head: *mut IfAddrs = std::ptr::null_mut();
        if unsafe { getifaddrs(&mut head) } != 0 || head.is_null() {
            return Vec::new();
        }
        let mut out = Vec::new();
        let mut p = head;
        while !p.is_null() {
            let ifa = unsafe { &*p };
            let name = if ifa.name.is_null() {
                ""
            } else {
                unsafe { CStr::from_ptr(ifa.name).to_str().unwrap_or("") }
            };
            if ifa.flags & IFF_UP != 0
                && ifa.flags & IFF_LOOPBACK == 0
                && keep_iface(name)
                && !ifa.addr.is_null()
            {
                let sa = unsafe { &*ifa.addr };
                if sa.family == AF_LINK {
                    let dl = unsafe { &*(ifa.addr as *const SockAddrDl) };
                    if dl.alen == 6 {
                        let off = dl.nlen as usize;
                        if off + 6 <= dl.data.len() {
                            let mut mac = [0u8; 6];
                            mac.copy_from_slice(&dl.data[off..off + 6]);
                            if !mac.iter().all(|b| *b == 0) {
                                let s = format_mac(&mac);
                                if !out.contains(&s) {
                                    out.push(s);
                                }
                            }
                        }
                    }
                }
            }
            p = ifa.next;
        }
        unsafe { freeifaddrs(head) };
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::time::Duration;

    #[test]
    fn parse_mac_accepts_colon_dash_and_bare() {
        let want = [0xaa, 0xbb, 0xcc, 0xdd, 0xee, 0xff];
        assert_eq!(parse_mac("aa:bb:cc:dd:ee:ff").unwrap(), want);
        assert_eq!(parse_mac("AA-BB-CC-DD-EE-FF").unwrap(), want);
        assert_eq!(parse_mac("aabbccddeeff").unwrap(), want);
        assert!(parse_mac("").is_err());
        assert!(parse_mac("aa:bb").is_err());
        assert!(parse_mac("00:00:00:00:00:00").is_err());
        assert_eq!(format_mac(&want), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn magic_packet_is_sync_plus_sixteen_macs() {
        let mac = parse_mac("01:23:45:67:89:ab").unwrap();
        let pkt = magic_packet(mac);
        assert_eq!(pkt.len(), 102);
        assert_eq!(&pkt[..6], &[0xff; 6]);
        for i in 0..16 {
            assert_eq!(&pkt[6 + i * 6..12 + i * 6], &mac);
        }
    }

    #[test]
    fn send_magic_reaches_local_udp_socket() {
        let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let addr = rx.local_addr().unwrap();
        rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
        let mac = parse_mac("aa:bb:cc:dd:ee:ff").unwrap();
        send_magic_to(mac, &addr.to_string()).unwrap();
        let mut buf = [0u8; 128];
        let (n, _) = rx.recv_from(&mut buf).unwrap();
        assert_eq!(n, 102);
        assert_eq!(&buf[..102], &magic_packet(mac));
    }
}
