//! Bounded structural preflight, not filesystem repair or proof of data integrity.
use crate::{Error, Quota, Result};

pub(crate) fn validate_superblock(bytes: &[u8; 1024], quota: Quota) -> Result<()> {
    let u16_at = |n| u16::from_le_bytes([bytes[n], bytes[n + 1]]);
    let u32_at = |n| u32::from_le_bytes([bytes[n], bytes[n + 1], bytes[n + 2], bytes[n + 3]]);
    let block_size = 1024u64.checked_shl(u32_at(24)).ok_or(Error::Quarantined)?;
    let blocks = u32_at(4) as u64 | ((u32_at(336) as u64) << 32);
    // A dirty-but-not-error filesystem may replay its journal in the kernel.
    // An ext4 error flag is quarantined; no root fsck is ever invoked.
    if u16_at(56) != 0xef53 || u16_at(58) & 2 != 0 || block_size != 4096
        || u32_at(0) == 0 || u32_at(0) as u64 > quota.inodes
        || blocks.checked_mul(block_size).ok_or(Error::Quarantined)? != quota.bytes {
        return Err(Error::Quarantined);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> ([u8; 1024], Quota) {
        let mut bytes = [0; 1024];
        bytes[0..4].copy_from_slice(&1024u32.to_le_bytes());
        bytes[4..8].copy_from_slice(&16384u32.to_le_bytes());
        bytes[24..28].copy_from_slice(&2u32.to_le_bytes());
        bytes[56..58].copy_from_slice(&0xef53u16.to_le_bytes());
        bytes[58..60].copy_from_slice(&1u16.to_le_bytes());
        (bytes, Quota { bytes: 64 * 1024 * 1024, inodes: 1024 })
    }
    #[test]
    fn ext4_refuses_wrong_magic_inode_rounding_and_size() {
        let (bytes, quota) = fixture();
        assert_eq!(validate_superblock(&bytes, quota), Ok(()));
        for offset in [0, 4, 24, 56] {
            let mut bad = bytes; bad[offset] ^= 1;
            assert_eq!(validate_superblock(&bad, quota), Err(Error::Quarantined));
        }
        let mut bad = bytes; bad[24..28].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(validate_superblock(&bad, quota), Err(Error::Quarantined));
        let mut bad = bytes; bad[336..340].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(validate_superblock(&bad, quota), Err(Error::Quarantined));
    }
    #[test]
    fn ext4_dirty_journal_can_replay_but_error_flag_is_quarantined() {
        let (mut bytes, quota) = fixture();
        bytes[58] = 0; assert_eq!(validate_superblock(&bytes, quota), Ok(()));
        bytes[58] = 2; assert_eq!(validate_superblock(&bytes, quota), Err(Error::Quarantined));
    }
}
