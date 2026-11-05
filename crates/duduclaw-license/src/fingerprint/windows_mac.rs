//! Windows selection retains adapter metadata until software adapters are removed.

fn eligible_adapter(if_type: u32, description: &str, name: &str) -> bool {
    // IF_TYPE_SOFTWARE_LOOPBACK (24), PROP_VIRTUAL (53), TUNNEL (131).
    if description.trim().is_empty() || matches!(if_type, 0 | 24 | 53 | 131) { return false; }
    let text = format!("{description} {name}").to_ascii_lowercase();
    !["loopback", "km-test", "km test", "npcap", "software-only", "software only"]
        .iter().any(|marker| text.contains(marker))
}

// Offsets come from the Windows ABI in production; synthetic layouts let the
// same bounds checks run on every host without dereferencing API pointers.
#[derive(Clone, Copy)]
struct AdapterLayout {
    next: usize,
    description: usize,
    name: usize,
    physical_address: usize,
    physical_address_length: usize,
    if_type: usize,
}

fn field<const N: usize>(record: &[u8], offset: usize) -> Option<[u8; N]> {
    record.get(offset..offset.checked_add(N)?)?.try_into().ok()
}

fn wide_string(buffer: &[u8], address: usize) -> Option<String> {
    let offset = address.checked_sub(buffer.as_ptr() as usize)?;
    if address % 2 != 0 { return None; }
    let mut units = Vec::new();
    for pair in buffer.get(offset..)?.chunks_exact(2) {
        let unit = u16::from_ne_bytes([pair[0], pair[1]]);
        if unit == 0 { return String::from_utf16(&units).ok(); }
        units.push(unit);
    }
    None // No terminator inside the returned buffer.
}

fn adapter_macs(buffer: &[u8], layout: AdapterLayout) -> Option<Vec<[u8; 6]>> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut visited = std::collections::HashSet::new();
    loop {
        if !visited.insert(offset) { return None; }
        let remaining = buffer.get(offset..)?;
        // Length is the first ULONG in every supported Windows structure.
        // Restrict all subsequent field reads to both Length and the buffer.
        let length = u32::from_ne_bytes(field(remaining, 0)?) as usize;
        let record = remaining.get(..length)?;
        let next = usize::from_ne_bytes(field(record, layout.next)?);
        let description = usize::from_ne_bytes(field(record, layout.description)?);
        let name = usize::from_ne_bytes(field(record, layout.name)?);
        let physical_address: [u8; 6] = field(record, layout.physical_address)?;
        let physical_address_length = u32::from_ne_bytes(field(record, layout.physical_address_length)?);
        let if_type = u32::from_ne_bytes(field(record, layout.if_type)?);
        // A null description is always rejected. A missing optional friendly
        // name is allowed, but a present invalid string rejects the adapter.
        let description = wide_string(buffer, description);
        let name = if name == 0 { Some(String::new()) } else { wide_string(buffer, name) };
        if let (Some(description), Some(name)) = (description, name) {
            if physical_address_length == 6 && eligible_adapter(if_type, &description, &name) {
                out.push(physical_address);
            }
        }
        if next == 0 { return Some(out); }
        offset = next.checked_sub(buffer.as_ptr() as usize)?;
    }
}

#[cfg(target_os = "windows")]
fn adapter_layout() -> AdapterLayout {
    use std::mem::offset_of;
    use winapi::um::iptypes::IP_ADAPTER_ADDRESSES_LH;
    AdapterLayout {
        next: offset_of!(IP_ADAPTER_ADDRESSES_LH, Next),
        description: offset_of!(IP_ADAPTER_ADDRESSES_LH, Description),
        name: offset_of!(IP_ADAPTER_ADDRESSES_LH, FriendlyName),
        physical_address: offset_of!(IP_ADAPTER_ADDRESSES_LH, PhysicalAddress),
        physical_address_length: offset_of!(IP_ADAPTER_ADDRESSES_LH, PhysicalAddressLength),
        if_type: offset_of!(IP_ADAPTER_ADDRESSES_LH, IfType),
    }
}

#[cfg(target_os = "windows")]
pub(super) fn eligible_macs() -> Vec<[u8; 6]> {
    use std::{mem::{size_of, MaybeUninit}, ptr};
    use winapi::{shared::{winerror::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS}, ws2def::AF_UNSPEC},
        um::{iphlpapi::GetAdaptersAddresses, iptypes::IP_ADAPTER_ADDRESSES_LH}};

    // Aligned, initialized storage owned until the returned bytes are parsed.
    // Retry boundedly if the adapter list grows between calls; failure is weak.
    let mut bytes = 15 * 1024u32;
    for _ in 0..3 {
        if bytes == 0 || bytes > 4 * 1024 * 1024 { return Vec::new(); }
        let supplied = bytes as usize;
        let count = supplied.div_ceil(size_of::<IP_ADAPTER_ADDRESSES_LH>());
        let mut buffer: Vec<MaybeUninit<IP_ADAPTER_ADDRESSES_LH>> = Vec::with_capacity(count);
        buffer.resize_with(count, MaybeUninit::uninit);
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: head is correctly aligned with at least `bytes` writable bytes.
        // Initialize in place, including padding, before exposing API output
        // as bytes; moving a zeroed structure need not preserve its padding.
        let result = unsafe {
            ptr::write_bytes(head.cast::<u8>(), 0, supplied);
            GetAdaptersAddresses(AF_UNSPEC as u32, 0, ptr::null_mut(), head, &mut bytes)
        };
        if result == ERROR_BUFFER_OVERFLOW { continue; }
        if result != ERROR_SUCCESS || bytes == 0 || bytes as usize > supplied { return Vec::new(); }
        // SAFETY: the supplied region was zero-initialized in place, and the successful API
        // call returned no more than the supplied length. Parsing uses only
        // checked byte slices; pointers from the API are never dereferenced.
        let returned = unsafe { std::slice::from_raw_parts(head.cast::<u8>(), bytes as usize) };
        return adapter_macs(returned, adapter_layout()).unwrap_or_default();
    }
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_layout() -> AdapterLayout {
        #[cfg(target_os = "windows")]
        { adapter_layout() }
        #[cfg(not(target_os = "windows"))]
        { AdapterLayout { next: 8, description: 16, name: 24,
            physical_address: 32, physical_address_length: 40, if_type: 44 } }
    }

    const INSTANCE_MAC: [u8; 6] = [0x02, 0x12, 0x34, 0x56, 0x78, 0x9a];

    fn put<const N: usize>(buffer: &mut [u8], offset: usize, value: [u8; N]) {
        buffer[offset..offset + N].copy_from_slice(&value);
    }

    fn fixture() -> (Vec<u8>, AdapterLayout) {
        let layout = test_layout();
        let length = layout.if_type + 4;
        let mut buffer = vec![0; length + 32];
        put(&mut buffer, 0, (length as u32).to_ne_bytes());
        put(&mut buffer, layout.if_type, 6u32.to_ne_bytes());
        put(&mut buffer, layout.physical_address_length, 6u32.to_ne_bytes());
        put(&mut buffer, layout.physical_address, INSTANCE_MAC);
        let description = buffer.as_ptr() as usize + length;
        put(&mut buffer, layout.description, description.to_ne_bytes());
        for (index, unit) in "Intel Ethernet".encode_utf16().enumerate() {
            put(&mut buffer, length + index * 2, unit.to_ne_bytes());
        }
        (buffer, layout)
    }

    #[test]
    fn requires_known_type_and_nonblank_description() {
        for description in ["", " ", "\t\r\n", "\u{2003}"] {
            assert!(!eligible_adapter(6, description, "Ethernet"));
        }
        assert!(!eligible_adapter(0, "Intel Ethernet", "Ethernet"));
        assert!(!eligible_adapter(0, "", ""));
        let (mut buffer, layout) = fixture();
        assert_eq!(adapter_macs(&buffer, layout), Some(vec![INSTANCE_MAC]));
        put(&mut buffer, layout.if_type, 0u32.to_ne_bytes());
        assert_eq!(adapter_macs(&buffer, layout), Some(vec![]));
    }

    #[test]
    fn rejects_short_structure_before_reading_metadata() {
        let (mut buffer, layout) = fixture();
        // Includes lengths cutting through Description, FriendlyName, MAC,
        // PhysicalAddressLength and IfType, even when backing bytes exist.
        for length in 0..layout.if_type + 4 {
            put(&mut buffer, 0, (length as u32).to_ne_bytes());
            assert_eq!(adapter_macs(&buffer, layout), None, "Length={length}");
        }
        put(&mut buffer, 0, u32::MAX.to_ne_bytes());
        assert_eq!(adapter_macs(&buffer, layout), None);
        assert_eq!(adapter_macs(&buffer[..3], layout), None);
    }

    #[test]
    fn rejects_null_empty_and_blank_description() {
        for description in [None, Some(""), Some(" \t\r\n")] {
            let (mut buffer, layout) = fixture();
            let start = layout.if_type + 4;
            buffer[start..].fill(0);
            match description {
                None => put(&mut buffer, layout.description, 0usize.to_ne_bytes()),
                Some(text) => for (index, unit) in text.encode_utf16().enumerate() {
                    put(&mut buffer, start + index * 2, unit.to_ne_bytes());
                },
            }
            assert_eq!(adapter_macs(&buffer, layout), Some(vec![]));
        }
    }

    #[test]
    fn bounds_and_strictly_decodes_both_strings() {
        for string_field in [test_layout().description, test_layout().name] {
            for invalid in 0..6 {
                let (mut buffer, layout) = fixture();
                let base = buffer.as_ptr() as usize;
                let end = base + buffer.len();
                let address = match invalid {
                    0 => base - 2,
                    1 => end,
                    2 => end + 2,
                    3 => { // Non-terminated string at the buffer boundary.
                        let last = buffer.len() - 2;
                        put(&mut buffer, last, (b'x' as u16).to_ne_bytes());
                        end - 2
                    },
                    4 => { // Unpaired surrogate followed by a terminator.
                        let last = buffer.len() - 4;
                        put(&mut buffer, last, 0xd800u16.to_ne_bytes());
                        end - 4
                    },
                    _ => base + 1, // Misaligned UTF-16 pointer.
                };
                put(&mut buffer, string_field, address.to_ne_bytes());
                assert_eq!(adapter_macs(&buffer, layout), Some(vec![]), "case={invalid}");
            }
        }
        let (buffer, layout) = fixture();
        // Terminator is in the allocation, but outside the returned slice.
        assert_eq!(adapter_macs(&buffer[..buffer.len() - 4], layout), Some(vec![]));
    }

    #[test]
    fn traverses_valid_links_and_skips_missing_metadata() {
        let (mut buffer, layout) = fixture();
        let length = layout.if_type + 4;
        let second = buffer.len();
        buffer.resize(second + length, 0);
        // Resizing can move the allocation; rebuild every embedded pointer.
        let base = buffer.as_ptr() as usize;
        put(&mut buffer, layout.description, (base + length).to_ne_bytes());
        buffer.copy_within(0..length, second);
        put(&mut buffer, layout.next, (base + second).to_ne_bytes());
        assert_eq!(adapter_macs(&buffer, layout), Some(vec![INSTANCE_MAC, INSTANCE_MAC]));
        put(&mut buffer, layout.description, 0usize.to_ne_bytes());
        assert_eq!(adapter_macs(&buffer, layout), Some(vec![INSTANCE_MAC]));
        put(&mut buffer, second + layout.if_type, 0u32.to_ne_bytes());
        assert_eq!(adapter_macs(&buffer, layout), Some(vec![]));
    }

    #[test]
    fn rejects_out_of_buffer_links_and_cycles() {
        for invalid in 0..4 {
            let (mut buffer, layout) = fixture();
            let base = buffer.as_ptr() as usize;
            let next = match invalid {
                0 => base,
                1 => base - 1,
                2 => base + buffer.len(),
                _ => base + buffer.len() - 1,
            };
            put(&mut buffer, layout.next, next.to_ne_bytes());
            assert_eq!(adapter_macs(&buffer, layout), None);
        }
    }

    #[test]
    fn excludes_software_types_and_loopback_descriptions() {
        for if_type in [24, 53, 131] {
            assert!(!eligible_adapter(if_type, "adapter", "Ethernet"));
        }
        for text in ["Microsoft KM-TEST Loopback Adapter", "Npcap Loopback Adapter",
            "LOOPBACK", "km-test", "Software-only adapter", "Software Only Adapter"]
        {
            assert!(!eligible_adapter(6, text, "Ethernet"));
            assert!(!eligible_adapter(6, "Ethernet", text));
        }
        for text in ["Intel Ethernet", "Microsoft Hyper-V Network Adapter", "VirtIO Ethernet"] {
            assert!(eligible_adapter(6, text, "Ethernet"));
        }
    }
}
