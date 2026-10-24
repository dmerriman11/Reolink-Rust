//! LAN camera discovery, as in the official client's "Scan Device(s) in LAN".
//!
//! Broadcasts a Song P2P `C2D_S` search to UDP 2015/2018 on every interface; each camera answers
//! `<D2C_S_R><uid>…</uid>[<bat><pct>…</pct></bat>]</D2C_S_R>` from its own address.
//! shortcut: only Song-capable cameras (battery/Wi-Fi models, verified on Argus) answer this; wired
//! cameras found only by the SDK's separate BCSDK_DeviceSearch need that protocol reversed too.

use neolink_core::bcudp::{decrypt_discovery, encode_discovery, xml::{C2dS, PortList, UdpXml}};
use serde::Serialize;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Found {
    pub uid: String,
    pub ip: IpAddr,
    pub battery: Option<u32>,
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let start = xml.find(&format!("<{name}>"))? + name.len() + 2;
    let end = start + xml[start..].find(&format!("</{name}>"))?;
    Some(xml[start..end].trim())
}

/// Parse a decrypted search reply; None for any other discovery message.
fn parse_reply(xml: &str, ip: IpAddr) -> Option<Found> {
    let body = tag(xml, "D2C_S_R")?;
    let uid = tag(body, "uid").filter(|u| !u.is_empty())?.to_string();
    let battery = tag(body, "pct").and_then(|p| p.parse().ok());
    Some(Found { uid, ip, battery })
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut ips = vec![Ipv4Addr::BROADCAST];
    // Windows sends 255.255.255.255 out of one interface only, so also hit each subnet's broadcast.
    for iface in get_if_addrs::get_if_addrs().unwrap_or_default() {
        if let get_if_addrs::IfAddr::V4(v4) = iface.addr
            && !v4.ip.is_loopback()
            && let Some(b) = v4.broadcast
        {
            ips.push(b);
        }
    }
    ips.sort();
    ips.dedup();
    ips.into_iter().flat_map(|ip| [2015, 2018].map(|p| SocketAddr::from((ip, p)))).collect()
}

pub async fn scan(wait: Duration) -> std::io::Result<Vec<Found>> {
    let sock = UdpSocket::bind("0.0.0.0:0").await?;
    sock.set_broadcast(true)?;
    let port = sock.local_addr()?.port() as u32;
    let tid = std::process::id() ^ 0x5ca1_ab1e;
    let pkt = encode_discovery(tid, UdpXml::C2dS(C2dS { to: PortList { port } }));
    let targets = broadcast_targets();

    let mut found: Vec<Found> = vec![];
    let mut buf = [0u8; 4096];
    let deadline = tokio::time::Instant::now() + wait;
    // Send twice: UDP broadcasts get dropped, and sleeping cameras may miss the first one.
    let mut resend = tokio::time::interval(wait / 2);
    loop {
        tokio::select! {
            _ = tokio::time::sleep_until(deadline) => break,
            _ = resend.tick() => {
                for t in &targets {
                    let _ = sock.send_to(&pkt, t).await; // unreachable subnets are fine to skip
                }
            }
            r = sock.recv_from(&mut buf) => {
                // Windows can surface an ICMP unreachable from an earlier send as a recv error; ignore it.
                let Ok((n, from)) = r else { continue };
                if let Some(f) = decrypt_discovery(&buf[..n]).and_then(|xml| parse_reply(&xml, from.ip()))
                    && !found.iter().any(|x| x.uid == f.uid)
                {
                    found.push(f);
                }
            }
        }
    }
    found.sort_by_key(|f| f.ip);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_search_replies() {
        let ip: IpAddr = "192.168.1.254".parse().unwrap();
        let battery = "<P2P><D2C_S_R><bat><low>0</low><pct>99</pct><c>2</c><p>1</p></bat><uid>9527000J3E0S1HZE</uid></D2C_S_R></P2P>";
        assert_eq!(parse_reply(battery, ip), Some(Found { uid: "9527000J3E0S1HZE".into(), ip, battery: Some(99) }));
        let plain = "<P2P><D2C_S_R><uid>9527000IOPAHRG7I</uid></D2C_S_R></P2P>";
        assert_eq!(parse_reply(plain, ip).unwrap().battery, None);
        assert_eq!(parse_reply("<P2P><D2C_C_R><uid>X</uid></D2C_C_R></P2P>", ip), None);
        assert_eq!(parse_reply("<P2P><D2C_S_R><uid></uid></D2C_S_R></P2P>", ip), None);
    }
}
