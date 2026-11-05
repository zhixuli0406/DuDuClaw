//! Bounded DNS-over-TCP codec for the dedicated broker; never a general proxy.
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use duduclaw_isolation::egress::EgressError;

const ID: u16 = 0xd0dc;
const MAX_RECORDS: usize = 512;

pub(super) fn query(host: &str, kind: u16) -> Result<Vec<u8>, EgressError> {
    if !matches!(kind, 1 | 28) { return Err(EgressError::InvalidRequest); }
    let mut packet = vec![0xd0, 0xdc, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0];
    if host.is_empty() || host.len() > 253 { return Err(EgressError::InvalidRequest); }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 || !label.is_ascii() { return Err(EgressError::InvalidRequest); }
        packet.push(label.len() as u8);
        packet.extend_from_slice(label.as_bytes());
    }
    packet.push(0);
    packet.extend_from_slice(&kind.to_be_bytes());
    packet.extend_from_slice(&1u16.to_be_bytes());
    Ok(packet)
}

fn short(packet: &[u8], pos: &mut usize) -> Result<u16, EgressError> {
    let bytes = packet.get(*pos..*pos + 2).ok_or(EgressError::Unavailable)?;
    *pos += 2;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn name(packet: &[u8], pos: &mut usize) -> Result<Vec<u8>, EgressError> {
    let mut cursor = *pos;
    let mut jumped = false;
    let mut expanded = Vec::new();
    for _ in 0..128 {
        let length = *packet.get(cursor).ok_or(EgressError::Unavailable)?;
        if length == 0 {
            if !jumped { *pos = cursor + 1; }
            return Ok(expanded);
        }
        if length & 0xc0 == 0xc0 {
            let low = *packet.get(cursor + 1).ok_or(EgressError::Unavailable)?;
            let pointer = ((length as usize & 0x3f) << 8) | low as usize;
            if pointer >= cursor { return Err(EgressError::Unavailable); }
            if !jumped { *pos = cursor + 2; jumped = true; }
            cursor = pointer;
            continue;
        }
        if length > 63 { return Err(EgressError::Unavailable); }
        cursor += 1;
        let label = packet.get(cursor..cursor + length as usize).ok_or(EgressError::Unavailable)?;
        if !expanded.is_empty() { expanded.push(b'.'); }
        expanded.extend(label.iter().map(u8::to_ascii_lowercase));
        if expanded.len() > 253 { return Err(EgressError::Unavailable); }
        cursor += length as usize;
    }
    Err(EgressError::Unavailable)
}

pub(super) fn answer(packet: &[u8], host: &str, kind: u16) -> Result<Vec<IpAddr>, EgressError> {
    if packet.len() < 12 || packet.len() > u16::MAX as usize { return Err(EgressError::Unavailable); }
    let mut pos = 0;
    if short(packet, &mut pos)? != ID { return Err(EgressError::Unavailable); }
    let flags = short(packet, &mut pos)?;
    // QR is set; opcode, truncation, reserved Z and rcode must be zero.
    if flags & 0xfa4f != 0x8000 { return Err(EgressError::Unavailable); }
    if short(packet, &mut pos)? != 1 { return Err(EgressError::Unavailable); }
    let answers = short(packet, &mut pos)? as usize;
    let authority = short(packet, &mut pos)? as usize;
    let additional = short(packet, &mut pos)? as usize;
    let total = answers + authority + additional;
    if total > MAX_RECORDS { return Err(EgressError::ResourceLimit); }
    if name(packet, &mut pos)? != host.as_bytes()
        || short(packet, &mut pos)? != kind || short(packet, &mut pos)? != 1 {
        return Err(EgressError::Unavailable);
    }
    let mut addresses = Vec::new();
    let mut cname = false;
    for index in 0..total {
        name(packet, &mut pos)?;
        let rr_type = short(packet, &mut pos)?;
        let class = short(packet, &mut pos)?;
        pos = pos.checked_add(4).ok_or(EgressError::Unavailable)?;
        let length = short(packet, &mut pos)? as usize;
        let data = packet.get(pos..pos + length).ok_or(EgressError::Unavailable)?;
        if index < answers {
            if class != 1 { return Err(EgressError::Unavailable); }
            match rr_type {
                1 => {
                    let bytes: [u8; 4] = data.try_into().map_err(|_| EgressError::Unavailable)?;
                    addresses.push(IpAddr::V4(Ipv4Addr::from(bytes)));
                }
                28 => {
                    let bytes: [u8; 16] = data.try_into().map_err(|_| EgressError::Unavailable)?;
                    addresses.push(IpAddr::V6(Ipv6Addr::from(bytes)));
                }
                5 | 39 => {
                    let mut end = pos;
                    name(packet, &mut end)?;
                    if end != pos + length { return Err(EgressError::Unavailable); }
                    cname = true;
                }
                _ => {}
            }
            if addresses.len() > 64 { return Err(EgressError::ResourceLimit); }
        }
        pos += length;
    }
    if pos != packet.len() || (cname && addresses.is_empty()) { return Err(EgressError::Unavailable); }
    Ok(addresses)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn reply() -> Vec<u8> {
        let mut bytes = query("example.com", 1).unwrap();
        bytes[2] = 0x81;
        bytes[3] = 0x80;
        bytes[7] = 1;
        bytes.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 8, 8, 8, 8]);
        bytes
    }
    #[test]
    fn dns_wire_question_binding_and_compressed_answer() {
        assert_eq!(answer(&reply(), "example.com", 1).unwrap(), vec!["8.8.8.8".parse::<IpAddr>().unwrap()]);
        assert!(answer(&reply(), "other.com", 1).is_err());
        assert!(answer(&reply(), "example.com", 28).is_err());
        assert!(query("example.com", 255).is_err());
    }
    #[test]
    fn dns_wire_rejects_every_truncation_trailing_bytes_and_bad_flags() {
        let bytes = reply();
        for length in 0..bytes.len() { assert!(answer(&bytes[..length], "example.com", 1).is_err()); }
        let mut invalid = bytes.clone();
        invalid.push(0);
        assert!(answer(&invalid, "example.com", 1).is_err());
        for (offset, mask) in [(2, 2), (2, 8), (3, 1), (3, 0x40)] {
            let mut invalid = bytes.clone(); invalid[offset] |= mask;
            assert!(answer(&invalid, "example.com", 1).is_err());
        }
    }
    #[test]
    fn dns_wire_rejects_compression_cycles_and_excess_addresses() {
        assert!(name(&[0xc0, 0], &mut 0).is_err());
        let mut bytes = query("example.com", 1).unwrap();
        bytes[2] = 0x81; bytes[3] = 0x80; bytes[7] = 65;
        for _ in 0..65 { bytes.extend_from_slice(&[0xc0, 12, 0, 1, 0, 1, 0, 0, 0, 60, 0, 4, 8, 8, 8, 8]); }
        assert_eq!(answer(&bytes, "example.com", 1), Err(EgressError::ResourceLimit));
    }
    #[test]
    fn dns_wire_aliases_need_final_addresses_and_allow_nodata_without_alias() {
        for kind in [5u16,39] {
            let mut bytes=query("example.com",1).unwrap();
            bytes[2]=0x81;bytes[3]=0x80;
            assert_eq!(answer(&bytes,"example.com",1).unwrap(),Vec::<IpAddr>::new());
            bytes[7]=1;
            bytes.extend_from_slice(&[0xc0,12]);bytes.extend_from_slice(&kind.to_be_bytes());
            bytes.extend_from_slice(&[0,1,0,0,0,60,0,8,5,b'a',b'l',b'i',b'a',b's',0xc0,12]);
            assert_eq!(answer(&bytes,"example.com",1),Err(EgressError::Unavailable));
            bytes[7]=2;
            bytes.extend_from_slice(&[5,b'a',b'l',b'i',b'a',b's',0xc0,12,0,1,0,1,0,0,0,60,0,4,8,8,8,8]);
            assert_eq!(answer(&bytes,"example.com",1).unwrap(),vec!["8.8.8.8".parse::<IpAddr>().unwrap()]);
        }
    }

}
