//! This module contains the protocol for dealing with UDP.
//!
//! There are three types of packets
//!
//! - Discovery
//! - Ack
//! - Data
//!
//! ---
//!
//! **Discovery**: Deals with setting up the initial connection and including their
//! connection IDs and the MTU
//!
//! ---
//!
//! **Ack**: Is sent after every packet is recieved
//!
//! ---
//!
//! **Data**: Contains a Bc packet payload. This is a stream and one Bc Packet may
//! be split accross multiple UDP Data packets
//!

pub(crate) mod codex;
mod crc;
/// Functions to deserialize udp packets
pub mod de;
/// Contains the model describing the top level structures
pub mod model;
/// Functions to serialize udp packets
pub mod ser;
/// Contains the udp related xml payloads
pub mod xml;
/// Constains routines to de/encrypt udp xml
mod xml_crypto;

/// Encode a discovery packet, e.g. a `C2D_S` LAN search broadcast.
pub fn encode_discovery(tid: u32, payload: xml::UdpXml) -> Vec<u8> {
    model::BcUdp::Discovery(model::UdpDiscovery { tid, payload })
        .serialize(Vec::new())
        .expect("serializing to a Vec cannot fail")
}

/// Decrypt a discovery packet's XML without parsing it, so replies neolink doesn't model can be read.
pub fn decrypt_discovery(buf: &[u8]) -> Option<String> {
    let le = |i: usize| buf.get(i..i + 4).map(|b| u32::from_le_bytes(std::convert::TryInto::try_into(b).unwrap()));
    if le(0)? != model::MAGIC_HEADER_UDP_NEGO || le(8)? != 1 {
        return None;
    }
    let (size, tid, checksum) = (le(4)? as usize, le(12)?, le(16)?);
    let enc = buf.get(20..20 + size)?;
    if crc::calc_crc(enc) != checksum {
        return None;
    }
    String::from_utf8(xml_crypto::decrypt(tid, enc)).ok()
}
