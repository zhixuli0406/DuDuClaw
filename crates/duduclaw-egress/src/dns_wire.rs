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
        // Preserve label boundaries for alias and SOA ancestry comparisons.
        if label.contains(&b'.') { return Err(EgressError::Unavailable); }
        if !expanded.is_empty() { expanded.push(b'.'); }
        expanded.extend(label.iter().map(u8::to_ascii_lowercase));
        if expanded.len() > 253 { return Err(EgressError::Unavailable); }
        cursor += length as usize;
    }
    Err(EgressError::Unavailable)
}

fn in_zone(target: &[u8], zone: &[u8]) -> bool {
    zone.is_empty() || target == zone || (target.len() > zone.len()
        && target.ends_with(zone) && target[target.len() - zone.len() - 1] == b'.')
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
    let mut aliases = Vec::new();
    let mut address_owners = Vec::new();
    let mut soa_owners = Vec::new();
    for index in 0..total {
        let owner = name(packet, &mut pos)?;
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
                    if kind == 1 {
                        addresses.push(IpAddr::V4(Ipv4Addr::from(bytes)));
                        address_owners.push(owner.clone());
                    }
                }
                28 => {
                    let bytes: [u8; 16] = data.try_into().map_err(|_| EgressError::Unavailable)?;
                    if kind == 28 {
                        addresses.push(IpAddr::V6(Ipv6Addr::from(bytes)));
                        address_owners.push(owner.clone());
                    }
                }
                5 | 39 => {
                    let mut end = pos;
                    let target = name(packet, &mut end)?;
                    if end != pos + length { return Err(EgressError::Unavailable); }
                    if rr_type == 39 && owner.is_empty() { return Err(EgressError::Unavailable); }
                    aliases.push((owner.clone(), rr_type, target));
                }
                _ => {}
            }
            if addresses.len() > 64 { return Err(EgressError::ResourceLimit); }
        } else if index < answers + authority && rr_type == 6 {
            if class != 1 { return Err(EgressError::Unavailable); }
            let mut end = pos;
            name(packet, &mut end)?; // MNAME
            name(packet, &mut end)?; // RNAME
            if end.checked_add(20) != Some(pos + length) { return Err(EgressError::Unavailable); }
            soa_owners.push(owner);
        }
        pos += length;
    }
    if pos != packet.len() { return Err(EgressError::Unavailable); }
    // Walk only the supplied chain, independent of RR order. Never issue follow-up queries.
    let mut target = host.as_bytes().to_vec();
    let mut visited = vec![target.clone()];
    let mut used = vec![false; aliases.len()];
    loop {
        let mut next = None;
        for (index, (owner, rr_type, destination)) in aliases.iter().enumerate() {
            let dname_applies = *rr_type == 39 && owner != &target && in_zone(&target, owner);
            if used[index] {
                // A DNAME whose replacement stays in its own subtree cannot terminate.
                if dname_applies { return Err(EgressError::Unavailable); }
                continue;
            }
            let candidate = if *rr_type == 5 && owner == &target {
                destination.clone()
            } else if dname_applies {
                let mut rewritten = target[..target.len() - owner.len()].to_vec();
                rewritten.extend_from_slice(destination);
                rewritten
            } else { continue; };
            if candidate.len() > 253 || next.as_ref().is_some_and(|value| value != &candidate) {
                return Err(EgressError::Unavailable);
            }
            next = Some(candidate);
            used[index] = true;
        }
        let Some(next) = next else { break; };
        if visited.contains(&next) { return Err(EgressError::Unavailable); }
        visited.push(next.clone());
        target = next;
    }
    if used.contains(&false) || address_owners.iter().any(|owner| owner != &target) {
        return Err(EgressError::Unavailable);
    }
    // Alias-only NOERROR is NODATA only with a covering authority SOA.
    // NXDOMAIN and TC were already refused by the header check.
    if !aliases.is_empty() && addresses.is_empty() && !soa_owners.iter().any(|owner| in_zone(&target, owner)) {
        return Err(EgressError::Unavailable);
    }
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
    fn dns_wire_aliases_without_soa_need_final_addresses_and_allow_nodata_without_alias() {
        for kind in [5u16] {
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

    fn wire_name(value: &str) -> Vec<u8> {
        let mut bytes = Vec::new();
        for label in value.split('.').filter(|label| !label.is_empty()) {
            bytes.push(label.len() as u8);
            bytes.extend_from_slice(label.as_bytes());
        }
        bytes.push(0);
        bytes
    }

    fn record(packet: &mut Vec<u8>, owner: &str, kind: u16, data: &[u8]) {
        packet.extend(wire_name(owner));
        packet.extend_from_slice(&kind.to_be_bytes());
        packet.extend_from_slice(&[0, 1, 0, 0, 0, 60]);
        packet.extend_from_slice(&(data.len() as u16).to_be_bytes());
        packet.extend_from_slice(data);
    }

    fn alias_reply(kind: u16, soa: Option<&str>, address: Option<&[u8]>) -> Vec<u8> {
        let mut packet = query("www.github.com", kind).unwrap();
        packet[2] = 0x81; packet[3] = 0x80;
        packet[7] = if address.is_some() { 2 } else { 1 };
        record(&mut packet, "www.github.com", 5, &wire_name("github.com"));
        if let Some(address) = address { record(&mut packet, "github.com", kind, address); }
        if let Some(zone) = soa {
            packet[9] = 1;
            // Both SOA names use the bounded decompressor, including compression.
            let mut data = vec![0xc0, 16, 0xc0, 16];
            data.extend_from_slice(&[0; 20]);
            record(&mut packet, zone, 6, &data);
        }
        packet
    }

    #[test]
    fn dns_wire_alias_nodata_requires_covering_authority_soa() {
        for kind in [1, 28] {
            for zone in ["github.com", "com", ""] {
                assert_eq!(answer(&alias_reply(kind, Some(zone), None), "www.github.com", kind), Ok(vec![]));
            }
            for zone in [None, Some("example.com"), Some("hub.com"), Some("www.github.com")] {
                assert_eq!(answer(&alias_reply(kind, zone, None), "www.github.com", kind), Err(EgressError::Unavailable));
            }
        }
    }

    #[test]
    fn dns_wire_alias_nodata_rejects_nxdomain_truncation_and_malformed_soa() {
        let bytes = alias_reply(28, Some("github.com"), None);
        for length in 0..bytes.len() {
            assert!(answer(&bytes[..length], "www.github.com", 28).is_err());
        }
        for (offset, mask) in [(3, 3), (2, 2)] {
            let mut invalid = bytes.clone(); invalid[offset] |= mask;
            assert_eq!(answer(&invalid, "www.github.com", 28), Err(EgressError::Unavailable));
        }
        let mut invalid = bytes.clone();
        invalid[9] = 0; invalid[11] = 1;
        assert_eq!(answer(&invalid, "www.github.com", 28), Err(EgressError::Unavailable));
        let mut invalid = bytes.clone();
        let data = invalid.len() - 24;
        invalid[data] = 0xff; invalid[data + 1] = 0xff;
        assert_eq!(answer(&invalid, "www.github.com", 28), Err(EgressError::Unavailable));
        let mut invalid = bytes;
        let data = invalid.len() - 24;
        invalid[data - 1] = 23; invalid.pop();
        assert_eq!(answer(&invalid, "www.github.com", 28), Err(EgressError::Unavailable));
        let mut nxdomain = query("www.github.com", 28).unwrap();
        nxdomain[2] = 0x81; nxdomain[3] = 0x83;
        assert_eq!(answer(&nxdomain, "www.github.com", 28), Err(EgressError::Unavailable));
    }

    #[test]
    fn dns_wire_alias_nodata_rejects_mismatched_and_cyclic_chains() {
        for owner in ["unrelated.com", "github.com"] {
            let mut bytes = query("www.github.com", 28).unwrap();
            bytes[2] = 0x81; bytes[3] = 0x80; bytes[7] = 1; bytes[9] = 1;
            record(&mut bytes, owner, 5, &wire_name("github.com"));
            record(&mut bytes, "github.com", 6, &[0; 22]);
            assert_eq!(answer(&bytes, "www.github.com", 28), Err(EgressError::Unavailable));
        }
        let mut bytes = alias_reply(28, Some("github.com"), None);
        bytes[7] = 2;
        let authority = bytes.split_off(query("www.github.com", 28).unwrap().len() + wire_name("www.github.com").len() + 10 + wire_name("github.com").len());
        record(&mut bytes, "github.com", 5, &wire_name("www.github.com"));
        bytes.extend(authority);
        assert_eq!(answer(&bytes, "www.github.com", 28), Err(EgressError::Unavailable));
    }

    #[test]
    fn dns_wire_nodata_multi_alias_and_dname_chains() {
        for dname in [false, true] {
            let mut packet = query("www.github.com", 28).unwrap();
            packet[2] = 0x81; packet[3] = 0x80; packet[7] = 2; packet[9] = 1;
            let final_target = if dname { "www.example.com" } else { "example.com" };
            // Deliberately put the second hop before the first.
            record(&mut packet, final_target, 5, &wire_name("final.example.net"));
            record(&mut packet, if dname { "github.com" } else { "www.github.com" },
                if dname { 39 } else { 5 }, &wire_name("example.com"));
            record(&mut packet, "example.net", 6, &[0; 22]);
            assert_eq!(answer(&packet, "www.github.com", 28), Ok(vec![]));
        }
    }

    #[test]
    fn dns_wire_nodata_rejects_conflicting_aliases_and_wrong_family_addresses() {
        let mut packet = alias_reply(28, None, None);
        packet[7] = 2;
        record(&mut packet, "www.github.com", 5, &wire_name("other.example"));
        assert_eq!(answer(&packet, "www.github.com", 28), Err(EgressError::Unavailable));
        let mut packet = alias_reply(28, None, None);
        packet[7] = 2;
        record(&mut packet, "github.com", 1, &[8, 8, 8, 8]);
        assert_eq!(answer(&packet, "www.github.com", 28), Err(EgressError::Unavailable));
        let mut packet = alias_reply(28, Some("github.com"), None);
        let class = packet.len() - 24 - 8;
        packet[class + 1] = 3;
        assert_eq!(answer(&packet, "www.github.com", 28), Err(EgressError::Unavailable));
    }

    #[test]
    fn dns_wire_dname_with_synthesized_cname_and_positive_alias_answer() {
        let mut packet = query("www.github.com", 28).unwrap();
        packet[2] = 0x81; packet[3] = 0x80; packet[7] = 2; packet[9] = 1;
        record(&mut packet, "github.com", 39, &wire_name("example.com"));
        record(&mut packet, "www.github.com", 5, &wire_name("www.example.com"));
        record(&mut packet, "example.com", 6, &[0; 22]);
        assert_eq!(answer(&packet, "www.github.com", 28), Ok(vec![]));
        assert_eq!(answer(&alias_reply(1, None, Some(&[8, 8, 8, 8])), "www.github.com", 1),
            Ok(vec!["8.8.8.8".parse::<IpAddr>().unwrap()]));
        let mut excessive = alias_reply(28, Some("github.com"), None);
        excessive[8..10].copy_from_slice(&(MAX_RECORDS as u16).to_be_bytes());
        assert_eq!(answer(&excessive, "www.github.com", 28), Err(EgressError::ResourceLimit));
    }

    #[test]
    fn dns_wire_dname_cannot_rewrite_its_owner_or_loop_inside_its_subtree() {
        for (owner, destination) in [("github.com", "sub.github.com"), ("www.github.com", "example.com")] {
            let mut packet = query("www.github.com", 28).unwrap();
            packet[2] = 0x81; packet[3] = 0x80; packet[7] = 1; packet[9] = 1;
            record(&mut packet, owner, 39, &wire_name(destination));
            record(&mut packet, "com", 6, &[0; 22]);
            assert_eq!(answer(&packet, "www.github.com", 28), Err(EgressError::Unavailable));
        }
    }

}
