//! Chunk-authenticated manual-recovery archive. No automatic gateway restore.
use std::io::{Read, Write};
use chacha20poly1305::{XChaCha20Poly1305, XNonce, KeyInit, aead::{Aead, Payload}};
use duduclaw_isolation::ids::RequestId;
use rand::{RngCore, rngs::OsRng};
use crate::Error;

const MAGIC: &[u8; 8] = b"DDMIGR01";
pub const CHUNK_BYTES: usize = 64 * 1024;
const TAG_BYTES: usize = 16;

fn nonce(prefix: &[u8; 16], sequence: u64) -> [u8; 24] {
    let mut value = [0; 24];
    value[..16].copy_from_slice(prefix);
    value[16..].copy_from_slice(&sequence.to_be_bytes());
    value
}
fn aad(header: &[u8; 40], sequence: u64, length: u32) -> Vec<u8> {
    let mut out = header.to_vec();
    out.extend_from_slice(&sequence.to_be_bytes());
    out.extend_from_slice(&length.to_be_bytes());
    out
}

/// The supplied key lives in the root trust store. It is never serialized here.
/// Caller writes to a new 0600 temporary file, fsyncs, verifies with `decrypt`,
/// then publishes atomically and fsyncs its parent directory.
pub struct ArchiveWriter<W: Write> {
    output: W, cipher: XChaCha20Poly1305, prefix: [u8; 16], header: [u8; 40],
    sequence: u64, total: u64, max_bytes: u64, poisoned: bool,
}
impl<W: Write> ArchiveWriter<W> {
    pub fn new(mut output: W, key: &[u8; 32], migration: RequestId, max_bytes: u64) -> Result<Self, Error> {
        if max_bytes == 0 { return Err(Error::Limit); }
        let mut prefix = [0; 16];
        OsRng.try_fill_bytes(&mut prefix).map_err(|_| Error::Unavailable)?;
        let mut header = [0; 40];
        header[..8].copy_from_slice(MAGIC);
        header[8..24].copy_from_slice(&migration.0);
        header[24..].copy_from_slice(&prefix);
        output.write_all(&header)?;
        Ok(Self { output, cipher: XChaCha20Poly1305::new(key.into()), prefix, header,
            sequence: 0, total: 0, max_bytes, poisoned: false })
    }
    fn frame(&mut self, bytes: &[u8]) -> Result<(), Error> {
        if self.poisoned { return Err(Error::Storage); }
        // A partial write must never permit reuse of this nonce, including when
        // the caller catches an I/O error and attempts another append.
        self.poisoned = true;
        let length = u32::try_from(bytes.len()).map_err(|_| Error::Limit)?;
        let iv = nonce(&self.prefix, self.sequence);
        let associated = aad(&self.header, self.sequence, length);
        let encrypted = self.cipher.encrypt(XNonce::from_slice(&iv), Payload { msg: bytes, aad: &associated })
            .map_err(|_| Error::Authentication)?;
        self.output.write_all(&length.to_be_bytes())?;
        self.output.write_all(&encrypted)?;
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Limit)?;
        self.poisoned = false;
        Ok(())
    }
    pub fn append(&mut self, bytes: &[u8]) -> Result<(), Error> {
        let total = self.total.checked_add(bytes.len() as u64).ok_or(Error::Limit)?;
        if total > self.max_bytes { return Err(Error::Limit); }
        for chunk in bytes.chunks(CHUNK_BYTES) { self.frame(chunk)?; }
        self.total = total;
        Ok(())
    }
    /// A terminal authenticated frame is mandatory even for empty archives.
    pub fn finish(mut self) -> Result<W, Error> {
        self.frame(&[])?;
        self.output.flush()?;
        Ok(self.output)
    }
}

/// Output MUST remain unpublished until this returns Ok: a truncated archive can
/// contain an authenticated prefix. Manual restore must stage into a fresh root
/// directory, never a live gateway, and discard the stage on any error.
pub fn decrypt(mut input: impl Read, mut staging: impl Write, key: &[u8; 32],
    migration: RequestId, max_bytes: u64) -> Result<u64, Error> {
    let mut header = [0; 40];
    input.read_exact(&mut header).map_err(|_| Error::Authentication)?;
    if &header[..8] != MAGIC || header[8..24] != migration.0 { return Err(Error::Authentication); }
    let prefix: [u8; 16] = header[24..].try_into().map_err(|_| Error::Invalid)?;
    let cipher = XChaCha20Poly1305::new(key.into());
    let mut sequence = 0u64;
    let mut total = 0u64;
    loop {
        let mut size = [0;4];
        input.read_exact(&mut size).map_err(|_| Error::Authentication)?;
        let length = u32::from_be_bytes(size);
        if length as usize > CHUNK_BYTES { return Err(Error::Limit); }
        total = total.checked_add(length as u64).ok_or(Error::Limit)?;
        if total > max_bytes { return Err(Error::Limit); }
        let mut encrypted = vec![0; length as usize + TAG_BYTES];
        input.read_exact(&mut encrypted).map_err(|_| Error::Authentication)?;
        let iv = nonce(&prefix, sequence);
        let associated = aad(&header, sequence, length);
        let plaintext = cipher.decrypt(XNonce::from_slice(&iv), Payload { msg: &encrypted, aad: &associated })
            .map_err(|_| Error::Authentication)?;
        if length == 0 {
            let mut trailing = [0; 1];
            if input.read(&mut trailing)? != 0 { return Err(Error::Authentication); }
            staging.flush()?;
            return Ok(total);
        }
        staging.write_all(&plaintext)?;
        sequence = sequence.checked_add(1).ok_or(Error::Limit)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn archive() -> Vec<u8> {
        let mut writer = ArchiveWriter::new(Vec::new(), &[4;32], RequestId([1;16]), 200_000).unwrap();
        writer.append(&vec![7; 140_000]).unwrap();
        writer.finish().unwrap()
    }
    #[test]
    fn encrypted_snapshot_roundtrip_and_random_nonces() {
        let bytes = archive();
        assert_ne!(bytes, archive());
        let mut recovered = Vec::new();
        assert_eq!(decrypt(bytes.as_slice(), &mut recovered, &[4;32], RequestId([1;16]), 200_000).unwrap(), 140_000);
        assert_eq!(recovered, vec![7; 140_000]);
    }
    #[test]
    fn corrupt_truncated_wrong_key_and_wrong_migration_fail() {
        let bytes = archive();
        for cut in [0, 39, 40, 41, bytes.len()-1, bytes.len()-20] {
            assert!(decrypt(&bytes[..cut], std::io::sink(), &[4;32], RequestId([1;16]), 200_000).is_err());
        }
        let mut changed = bytes.clone(); changed[55] ^= 1;
        assert!(decrypt(changed.as_slice(), std::io::sink(), &[4;32], RequestId([1;16]), 200_000).is_err());
        assert!(decrypt(bytes.as_slice(), std::io::sink(), &[5;32], RequestId([1;16]), 200_000).is_err());
        assert!(decrypt(bytes.as_slice(), std::io::sink(), &[4;32], RequestId([2;16]), 200_000).is_err());
        assert!(decrypt(bytes.as_slice(), std::io::sink(), &[4;32], RequestId([1;16]), 10).is_err());
        let mut trailing = bytes; trailing.push(0);
        assert!(decrypt(trailing.as_slice(), std::io::sink(), &[4;32], RequestId([1;16]), 200_000).is_err());
    }
    #[test]
    fn append_budget_and_empty_archive() {
        let mut writer = ArchiveWriter::new(Vec::new(), &[4;32], RequestId([1;16]), 1).unwrap();
        assert_eq!(writer.append(&[1,2]), Err(Error::Limit));
        let bytes = writer.finish().unwrap();
        assert_eq!(decrypt(bytes.as_slice(), std::io::sink(), &[4;32], RequestId([1;16]), 1).unwrap(), 0);
    }
    #[test]
    fn partial_write_poisons_writer_before_nonce_can_be_reused() {
        struct Broken { written: usize }
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.written >= 44 { return Err(std::io::ErrorKind::WriteZero.into()); }
                let count = bytes.len().min(44 - self.written);
                self.written += count;
                Ok(count)
            }
            fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
        }
        let mut writer = ArchiveWriter::new(Broken { written: 0 }, &[4;32], RequestId([1;16]), 100).unwrap();
        assert_eq!(writer.append(b"first"), Err(Error::Storage));
        assert_eq!(writer.append(b"second"), Err(Error::Storage));
        assert!(writer.finish().is_err());
    }
}
