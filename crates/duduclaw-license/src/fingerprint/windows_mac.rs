//! Windows selection retains adapter metadata until software adapters are removed.

fn eligible_adapter(if_type: u32, description: &str, name: &str) -> bool {
    // IF_TYPE_SOFTWARE_LOOPBACK (24), PROP_VIRTUAL (53), TUNNEL (131).
    if matches!(if_type, 24 | 53 | 131) { return false; }
    let text = format!("{description} {name}").to_ascii_lowercase();
    !["loopback", "km-test", "km test", "npcap", "software-only", "software only"]
        .iter().any(|marker| text.contains(marker))
}

#[cfg(target_os = "windows")]
pub(super) fn eligible_macs() -> Vec<[u8; 6]> {
    use std::{mem::{size_of, MaybeUninit}, ptr};
    use winapi::{shared::{winerror::{ERROR_BUFFER_OVERFLOW, ERROR_SUCCESS}, ws2def::AF_UNSPEC},
        um::{iphlpapi::GetAdaptersAddresses, iptypes::IP_ADAPTER_ADDRESSES_LH}};

    // Aligned storage owned until every returned adapter/string has been read.
    // Retry boundedly if the adapter list grows between calls; failure is weak.
    let mut bytes = 15 * 1024u32;
    for _ in 0..3 {
        if bytes == 0 || bytes > 4 * 1024 * 1024 { return Vec::new(); }
        let count = (bytes as usize).div_ceil(size_of::<IP_ADAPTER_ADDRESSES_LH>());
        let mut buffer: Vec<MaybeUninit<IP_ADAPTER_ADDRESSES_LH>> = Vec::with_capacity(count);
        // Zero-fill fields a shorter OS structure version may leave untouched.
        buffer.resize_with(count, MaybeUninit::zeroed);
        let head = buffer.as_mut_ptr().cast::<IP_ADAPTER_ADDRESSES_LH>();
        // SAFETY: head is correctly aligned with at least `bytes` writable bytes.
        let result = unsafe { GetAdaptersAddresses(AF_UNSPEC as u32, 0,
            ptr::null_mut(), head, &mut bytes) };
        if result == ERROR_BUFFER_OVERFLOW { continue; }
        if result != ERROR_SUCCESS { return Vec::new(); }
        let mut out = Vec::new();
        let mut adapter = head;
        while !adapter.is_null() {
            // SAFETY: on success the API supplies a linked list and terminated
            // UTF-16 strings inside the still-live buffer. Read unaligned for x86.
            let item = unsafe { adapter.read_unaligned() };
            let description = unsafe { wide_string(item.Description) };
            let name = unsafe { wide_string(item.FriendlyName) };
            if item.PhysicalAddressLength == 6 && eligible_adapter(item.IfType, &description, &name) {
                let mut mac = [0; 6];
                mac.copy_from_slice(&item.PhysicalAddress[..6]);
                out.push(mac);
            }
            adapter = item.Next;
        }
        return out;
    }
    Vec::new()
}

#[cfg(target_os = "windows")]
unsafe fn wide_string(value: *const u16) -> String {
    if value.is_null() { return String::new(); }
    let mut len = 0;
    // SAFETY: caller supplies a Windows API-owned terminated UTF-16 string.
    while unsafe { *value.add(len) } != 0 { len += 1; }
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(value, len) })
}

#[cfg(test)]
mod tests {
    use super::*;

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
