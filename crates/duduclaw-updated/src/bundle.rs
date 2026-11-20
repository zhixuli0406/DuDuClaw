//! Migration bundle (plan §3.2): a strict reader for the uncompressed ustar image
//! held in the verifier's sealed memfd, the root-threshold `BundleAuthorization`
//! and the legacy release minisign signature. Pure functions over bytes; the
//! caller owns every file descriptor and never reopens the bundle by path.
use std::collections::BTreeMap;
use duduclaw_isolation::{ids::Sha256Digest, update::*};
use crate::{Error, Result, metadata::{decode_canonical, MAX_METADATA_BYTES}};

pub const ROOT_ENTRY: &str = "root.json";
pub const ROOT_SIGNATURES_ENTRY: &str = "root.json.signatures.json";
pub const MANIFEST_ENTRY: &str = "manifest.json";
pub const MANIFEST_SIGNATURES_ENTRY: &str = "manifest.json.signatures.json";
const ARTIFACT_PREFIX: &str = "artifacts/";
const BLOCK: usize = 512;

/// Hard bounds applied while parsing, before any manifest is authenticated.
/// Exact artifact sizes are enforced afterwards by [`BundleEntries::check_manifest`].
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// Bound on tar headers of any type (every header counts, not only members).
    pub max_entries: usize,
    pub max_metadata_bytes: usize,
    pub max_signature_bytes: usize,
    pub max_artifact_bytes: usize,
}
impl Limits {
    /// `MAX_ARTIFACTS + 4` entries; the per-artifact bound equals the publication
    /// policy's 512 MiB ceiling (`publication::MAX_ARTIFACT_BYTES`).
    pub const BOOTSTRAP: Limits = Limits { max_entries: MAX_ARTIFACTS + 4,
        max_metadata_bytes: MAX_METADATA_BYTES, max_signature_bytes: 64 * 1024,
        max_artifact_bytes: 512 * 1024 * 1024 };
}

/// Byte range of one regular-file member inside the image.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Entry { pub offset: usize, pub size: usize }

#[derive(Debug)]
pub struct BundleEntries {
    pub root: Entry,
    pub root_signatures: Entry,
    pub manifest: Entry,
    pub manifest_signatures: Entry,
    /// Keyed by artifact id (`artifacts/<id>` in the tar).
    pub artifacts: BTreeMap<String, Entry>,
}
impl BundleEntries {
    pub fn data(image: &[u8], entry: Entry) -> Result<&[u8]> {
        let end = entry.offset.checked_add(entry.size).ok_or(Error::Invalid)?;
        image.get(entry.offset..end).ok_or(Error::Invalid)
    }
    /// The tar must carry exactly the authenticated manifest's artifacts, each with
    /// exactly the signed size. Hashes are checked when the bytes are staged.
    pub fn check_manifest(&self, manifest: &ReleaseManifest) -> Result<()> {
        if self.artifacts.len() != manifest.artifacts.len() { return Err(Error::Signature); }
        for artifact in &manifest.artifacts {
            let entry = self.artifacts.get(&artifact.id).ok_or(Error::Signature)?;
            if u64::try_from(entry.size).map_err(|_| Error::Signature)? != artifact.size_bytes {
                return Err(Error::Signature);
            }
        }
        Ok(())
    }
}

fn block(image: &[u8], offset: usize) -> Result<&[u8]> {
    image.get(offset..offset.checked_add(BLOCK).ok_or(Error::Invalid)?).ok_or(Error::Invalid)
}
fn field(header: &[u8], start: usize, end: usize) -> Result<&[u8]> { header.get(start..end).ok_or(Error::Invalid) }

/// POSIX octal: optional leading spaces, at least one digit, then NUL/space padding.
/// GNU base-256 (high bit) and any other byte are refused.
fn octal(bytes: &[u8]) -> Result<u64> {
    let mut value: u64 = 0;
    let mut digits = 0usize;
    let mut leading = true;
    let mut ended = false;
    for &byte in bytes {
        if ended {
            if byte != 0 && byte != b' ' { return Err(Error::Invalid); }
            continue;
        }
        match byte {
            b' ' if leading => continue,
            b'0'..=b'7' => {
                leading = false;
                digits += 1;
                value = value.checked_mul(8).and_then(|v| v.checked_add(u64::from(byte - b'0'))).ok_or(Error::Invalid)?;
            }
            0 | b' ' => ended = true,
            _ => return Err(Error::Invalid),
        }
    }
    if digits == 0 { return Err(Error::Invalid); }
    Ok(value)
}

/// NUL-terminated ASCII text; every byte after the terminator must be NUL.
fn text(bytes: &[u8]) -> Result<&str> {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    let (value, rest) = bytes.split_at(end);
    if rest.iter().any(|b| *b != 0) || !value.is_ascii() { return Err(Error::Invalid); }
    std::str::from_utf8(value).map_err(|_| Error::Invalid)
}

fn checksum_ok(header: &[u8]) -> Result<()> {
    let expected = octal(field(header, 148, 156)?)?;
    let actual: u64 = header.iter().enumerate()
        .map(|(index, byte)| if (148..156).contains(&index) { 32 } else { u64::from(*byte) }).sum();
    if expected != actual { return Err(Error::Invalid); }
    Ok(())
}

fn header_name(header: &[u8]) -> Result<String> {
    let name = text(field(header, 0, 100)?)?;
    let prefix = text(field(header, 345, 500)?)?;
    if name.is_empty() { return Err(Error::Invalid); }
    Ok(if prefix.is_empty() { name.to_owned() } else { format!("{prefix}/{name}") })
}

/// Same identifier rule as `ReleaseManifest::validate` artifact ids.
fn artifact_id(name: &str) -> Option<&str> {
    let id = name.strip_prefix(ARTIFACT_PREFIX)?;
    let ok = !id.is_empty() && id.len() <= 128 && id != "." && id != ".."
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'));
    ok.then_some(id)
}

/// Parse the whole image. The bundle format (plan §3.2) is plain uncompressed
/// ustar, so only regular-file headers with whitelisted names are accepted: no
/// pax headers of any kind (extended or global), no links, devices, directories,
/// FIFOs or GNU extensions; no duplicates; every header counts toward
/// `max_entries`; bounded sizes; zero padding; two-or-more zero blocks and
/// nothing else at the end.
pub fn read_ustar(image: &[u8], limits: &Limits) -> Result<BundleEntries> {
    let mut offset = 0usize;
    let mut metadata: [Option<Entry>; 4] = [None; 4];
    let mut artifacts = BTreeMap::new();
    let mut headers = 0usize;
    loop {
        let header = block(image, offset)?;
        if header.iter().all(|b| *b == 0) {
            let rest = image.get(offset..).ok_or(Error::Invalid)?;
            if rest.len() < 2 * BLOCK || rest.iter().any(|b| *b != 0) { return Err(Error::Invalid); }
            break;
        }
        // Counted before anything else about the header is interpreted.
        headers = headers.checked_add(1).ok_or(Error::Invalid)?;
        if headers > limits.max_entries { return Err(Error::Invalid); }
        checksum_ok(header)?;
        if field(header, 257, 263)? != b"ustar\0" || field(header, 263, 265)? != b"00" { return Err(Error::Invalid); }
        if field(header, 157, 257)?.iter().any(|b| *b != 0) { return Err(Error::Invalid); }
        let declared = octal(field(header, 124, 136)?)?;
        let typeflag = *header.get(156).ok_or(Error::Invalid)?;
        if typeflag != b'0' && typeflag != 0 { return Err(Error::Invalid); }
        let start = offset.checked_add(BLOCK).ok_or(Error::Invalid)?;
        let name = header_name(header)?;
        let size = usize::try_from(declared).map_err(|_| Error::Invalid)?;
        let entry = Entry { offset: start, size };
        let (slot, limit) = match name.as_str() {
            ROOT_ENTRY => (Some(0), limits.max_metadata_bytes),
            ROOT_SIGNATURES_ENTRY => (Some(1), limits.max_signature_bytes),
            MANIFEST_ENTRY => (Some(2), limits.max_metadata_bytes),
            MANIFEST_SIGNATURES_ENTRY => (Some(3), limits.max_signature_bytes),
            other => {
                artifact_id(other).ok_or(Error::Invalid)?;
                (None, limits.max_artifact_bytes)
            }
        };
        if size > limit { return Err(Error::Invalid); }
        BundleEntries::data(image, entry)?;
        match slot {
            Some(index) => {
                let target = metadata.get_mut(index).ok_or(Error::Invalid)?;
                if target.replace(entry).is_some() { return Err(Error::Invalid); }
            }
            None => {
                let id = artifact_id(&name).ok_or(Error::Invalid)?.to_owned();
                if artifacts.insert(id, entry).is_some() { return Err(Error::Invalid); }
            }
        }
        let padded = size.checked_add(BLOCK - 1).ok_or(Error::Invalid)? / BLOCK * BLOCK;
        let next = start.checked_add(padded).ok_or(Error::Invalid)?;
        let padding = image.get(start.checked_add(size).ok_or(Error::Invalid)?..next).ok_or(Error::Invalid)?;
        if padding.iter().any(|b| *b != 0) { return Err(Error::Invalid); }
        offset = next;
    }
    let [root, root_signatures, manifest, manifest_signatures] = metadata;
    Ok(BundleEntries { root: root.ok_or(Error::Invalid)?, root_signatures: root_signatures.ok_or(Error::Invalid)?,
        manifest: manifest.ok_or(Error::Invalid)?, manifest_signatures: manifest_signatures.ok_or(Error::Invalid)?,
        artifacts })
}

/// Transition dual signature, half two (plan §3.2): canonical `BundleAuthorization`
/// with root threshold under the fixed migration-bundle namespace, bound to the
/// exact bundle digest/size, the verifier's version and the host architecture.
#[allow(clippy::too_many_arguments)]
pub fn verify_authorization(root: &RootMetadata, bytes: &[u8], armors: &[String], bundle_sha256: Sha256Digest,
    bundle_size: u64, arch: Architecture, version: ReleaseVersion, now: u64) -> Result<BundleAuthorization> {
    crate::trust::validate_root(root, now)?;
    let authorization: BundleAuthorization = decode_canonical(bytes)?;
    authorization.validate()?;
    let signatures = crate::trust::signatures(armors)?;
    crate::trust::reject_unknown(&signatures, &[root])?;
    crate::trust::threshold(root, &signatures, MIGRATION_BUNDLE_SSHSIG_NAMESPACE, bytes)?;
    if now >= authorization.expires_at_unix { return Err(Error::Expired); }
    if authorization.bundle_sha256 != bundle_sha256 || authorization.bundle_size != bundle_size {
        return Err(Error::Signature);
    }
    if authorization.arch != arch || authorization.version != version { return Err(Error::Incompatible); }
    Ok(authorization)
}

/// The exact `.minisig` text minisign writes: four non-empty `\n`-terminated
/// lines (untrusted comment, signature, trusted comment, global signature) and
/// nothing after the fourth newline. No CR or other control byte, no empty line;
/// both signature lines are standard base64 text. `Signature::decode` alone would
/// ignore trailing lines, so this runs first.
pub(crate) fn strict_signature_text(text: &str) -> Result<()> {
    let body = text.strip_suffix('\n').ok_or(Error::Signature)?;
    let lines: Vec<&str> = body.split('\n').collect();
    let [untrusted, signature, trusted, global] = *lines.as_slice() else { return Err(Error::Signature) };
    let printable = |line: &str| !line.is_empty() && line.bytes().all(|b| (0x20..0x7f).contains(&b) || b >= 0x80);
    let base64 = |line: &str| !line.is_empty()
        && line.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
    if !lines.iter().copied().all(printable) || !base64(signature) || !base64(global)
        || !untrusted.starts_with("untrusted comment: ") || !trusted.starts_with("trusted comment: ") {
        return Err(Error::Signature);
    }
    Ok(())
}

/// Transition dual signature, half one: the legacy release minisign key. Legacy
/// (non-prehashed) signatures are refused, as in the gateway updater, and the
/// trusted comment must be exactly the one the release pipeline signs.
pub fn verify_legacy_signature(data: &[u8], signature: &str, public_key: &str, trusted_comment: &str) -> Result<()> {
    strict_signature_text(signature)?;
    let key = minisign_verify::PublicKey::from_base64(public_key).map_err(|_| Error::Signature)?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|_| Error::Signature)?;
    key.verify(data, &signature, false).map_err(|_| Error::Signature)?;
    if signature.trusted_comment() != trusted_comment { return Err(Error::Signature); }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn seal_checksum(header: &mut [u8; 512]) {
        header[148..156].copy_from_slice(b"        ");
        let sum: u32 = header.iter().map(|b| u32::from(*b)).sum();
        header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    }
    pub(crate) fn header(name: &str, size: usize, typeflag: u8) -> [u8; 512] {
        let mut header = [0u8; 512];
        header[..name.len()].copy_from_slice(name.as_bytes());
        header[100..108].copy_from_slice(b"0000600\0");
        header[108..116].copy_from_slice(b"0000000\0");
        header[116..124].copy_from_slice(b"0000000\0");
        header[124..136].copy_from_slice(format!("{size:011o}\0").as_bytes());
        header[136..148].copy_from_slice(b"00000000000\0");
        header[156] = typeflag;
        header[257..263].copy_from_slice(b"ustar\0");
        header[263..265].copy_from_slice(b"00");
        seal_checksum(&mut header);
        header
    }
    pub(crate) fn append_raw(out: &mut Vec<u8>, header: [u8; 512], data: &[u8]) {
        out.extend_from_slice(&header);
        out.extend_from_slice(data);
        out.resize(out.len() + (BLOCK - data.len() % BLOCK) % BLOCK, 0);
    }
    pub(crate) fn append(out: &mut Vec<u8>, name: &str, data: &[u8]) {
        append_raw(out, header(name, data.len(), b'0'), data);
    }
    pub(crate) fn finish(mut out: Vec<u8>) -> Vec<u8> {
        // Python's tarfile pads to a 10 KiB record; any amount of zeros is accepted.
        out.resize(out.len() + 10 * BLOCK, 0);
        out
    }
    pub(crate) fn tar(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, data) in entries { append(&mut out, name, data); }
        finish(out)
    }
    static UPDATER: [u8; 1500] = [7; 1500];
    fn inbox_entries() -> Vec<(&'static str, &'static [u8])> {
        vec![(ROOT_ENTRY, b"{\"root\":1}".as_slice()), (ROOT_SIGNATURES_ENTRY, b"[]".as_slice()),
            (MANIFEST_ENTRY, b"{\"manifest\":1}".as_slice()), (MANIFEST_SIGNATURES_ENTRY, b"[\"sig\"]".as_slice()),
            ("artifacts/gateway", b"gateway-bytes".as_slice()), ("artifacts/duduclaw-updated", UPDATER.as_slice())]
    }
    fn pax_record(key: &str, value: &str) -> Vec<u8> {
        // The length prefix counts itself; fixed-point iteration over digit count.
        let body = format!(" {key}={value}\n");
        let mut length = body.len() + 1;
        while format!("{length}{body}").len() != length { length += 1; }
        format!("{length}{body}").into_bytes()
    }
    fn pax_then(records: &[u8], name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for (entry_name, entry_data) in inbox_entries() { append(&mut out, entry_name, entry_data); }
        append_raw(&mut out, header("PaxHeader/x", records.len(), b'x'), records);
        append(&mut out, name, data);
        finish(out)
    }

    #[test]
    fn ustar_accepts_exact_inbox() {
        let image = tar(&inbox_entries());
        let entries = read_ustar(&image, &Limits::BOOTSTRAP).unwrap();
        assert_eq!(BundleEntries::data(&image, entries.root).unwrap(), b"{\"root\":1}");
        assert_eq!(BundleEntries::data(&image, entries.manifest_signatures).unwrap(), b"[\"sig\"]");
        assert_eq!(BundleEntries::data(&image, entries.artifacts["duduclaw-updated"]).unwrap(), &[7u8; 1500]);
        assert_eq!(entries.artifacts.len(), 2);
        // The ustar prefix field names an allowed member like the plain name does.
        let mut out = Vec::new();
        for (name, data) in inbox_entries().into_iter().take(5) { append(&mut out, name, data); }
        let mut split = header("duduclaw-updated", 3, b'0');
        split[345..354].copy_from_slice(b"artifacts");
        seal_checksum(&mut split);
        append_raw(&mut out, split, b"abc");
        let entries = read_ustar(&finish(out), &Limits::BOOTSTRAP).unwrap();
        assert_eq!(entries.artifacts["duduclaw-updated"].size, 3);
        // The bundle is plain ustar: even a pax header carrying only path or size is refused.
        let image = pax_then(&pax_record("path", "artifacts/extra"), "ignored-by-pax", b"xyz");
        assert!(read_ustar(&image, &Limits::BOOTSTRAP).is_err());
        let image = pax_then(&pax_record("size", "3"), "artifacts/extra", b"xyz");
        assert!(read_ustar(&image, &Limits::BOOTSTRAP).is_err());
        // Exact manifest correspondence: ids and signed sizes.
        let mut manifest = crate::publication::tests::manifest();
        manifest.artifacts.truncate(2);
        manifest.artifacts[0].id = "gateway".into(); manifest.artifacts[0].size_bytes = 13;
        manifest.artifacts[1].id = "duduclaw-updated".into(); manifest.artifacts[1].size_bytes = 1500;
        let entries = read_ustar(&tar(&inbox_entries()), &Limits::BOOTSTRAP).unwrap();
        entries.check_manifest(&manifest).unwrap();
        let mut wrong = manifest.clone(); wrong.artifacts[1].size_bytes = 1499;
        assert!(entries.check_manifest(&wrong).is_err());
        let mut missing = manifest.clone(); missing.artifacts.truncate(1);
        assert!(entries.check_manifest(&missing).is_err());
        let mut renamed = manifest; renamed.artifacts[0].id = "spawnd".into();
        assert!(entries.check_manifest(&renamed).is_err());
    }

    #[test]
    fn ustar_rejects_links_devices_pax_duplicates_oversize() {
        let base = inbox_entries();
        // Every non-regular type, including GNU long names and global pax headers.
        for typeflag in [b'1', b'2', b'3', b'4', b'5', b'6', b'7', b'L', b'K', b'g', b'S', b'V'] {
            let mut out = Vec::new();
            for (name, data) in &base { append(&mut out, name, data); }
            append_raw(&mut out, header("artifacts/link", 0, typeflag), b"");
            assert!(read_ustar(&finish(out), &Limits::BOOTSTRAP).is_err(), "type {typeflag}");
        }
        // A regular file that carries a link target is refused too.
        let mut out = Vec::new();
        for (name, data) in &base { append(&mut out, name, data); }
        let mut linked = header("artifacts/linked", 1, b'0');
        linked[157..168].copy_from_slice(b"/etc/shadow");
        seal_checksum(&mut linked);
        append_raw(&mut out, linked, b"x");
        assert!(read_ustar(&finish(out), &Limits::BOOTSTRAP).is_err());
        // Pax: only path/size, once each, never chained, never naming a forbidden member.
        for records in [pax_record("mtime", "1"), pax_record("linkpath", "/etc"), pax_record("uid", "0"),
            [pax_record("path", "artifacts/a"), pax_record("path", "artifacts/b")].concat(),
            pax_record("path", "../etc/shadow"), pax_record("path", "artifacts/../x"),
            pax_record("size", "4"), b"9 path=x\n".to_vec(), b"garbage".to_vec()] {
            assert!(read_ustar(&pax_then(&records, "artifacts/extra", b"xyz"), &Limits::BOOTSTRAP).is_err(),
                "{}", String::from_utf8_lossy(&records));
        }
        let mut out = Vec::new();
        for (name, data) in &base { append(&mut out, name, data); }
        let record = pax_record("path", "artifacts/a");
        append_raw(&mut out, header("PaxHeader/a", record.len(), b'x'), &record);
        append_raw(&mut out, header("PaxHeader/b", record.len(), b'x'), &record);
        append(&mut out, "artifacts/a", b"a");
        assert!(read_ustar(&finish(out), &Limits::BOOTSTRAP).is_err());
        // Duplicates of metadata and artifacts; unknown or unanchored names.
        for extra in ["root.json", "manifest.json.signatures.json", "artifacts/gateway", "evil.json", "./root.json",
            "artifacts/", "artifacts/../root.json", "artifacts/a/b", "/root.json", "artifacts/sp ace", "ARTIFACTS/x"] {
            let mut entries = base.clone();
            entries.push((extra, b"x".as_slice()));
            assert!(read_ustar(&tar(&entries), &Limits::BOOTSTRAP).is_err(), "{extra}");
        }
        // Missing metadata member.
        for skip in 0..4 {
            let entries: Vec<_> = base.iter().enumerate().filter(|(i, _)| *i != skip).map(|(_, e)| *e).collect();
            assert!(read_ustar(&tar(&entries), &Limits::BOOTSTRAP).is_err());
        }
        // Size and count bounds.
        let small = Limits { max_entries: 6, max_metadata_bytes: 14, max_signature_bytes: 7, max_artifact_bytes: 1500 };
        assert!(read_ustar(&tar(&base), &small).is_ok());
        for limits in [Limits { max_entries: 5, ..small }, Limits { max_metadata_bytes: 13, ..small },
            Limits { max_signature_bytes: 6, ..small }, Limits { max_artifact_bytes: 1499, ..small }] {
            assert!(read_ustar(&tar(&base), &limits).is_err());
        }
        let image = tar(&base);
        // Truncation anywhere, missing end blocks, trailing data, nonzero padding,
        // checksum, magic and numeric encodings.
        for end in [0, 100, 512, 1024, 1536, image.len() - 10 * BLOCK, image.len() - 9 * BLOCK - 1] {
            assert!(read_ustar(&image[..end], &Limits::BOOTSTRAP).is_err(), "truncated at {end}");
        }
        let mut trailing = image.clone(); *trailing.last_mut().unwrap() = 1;
        assert!(read_ustar(&trailing, &Limits::BOOTSTRAP).is_err());
        let mut padding = image.clone(); padding[512 + 11] = 1; // inside root.json's block padding
        assert!(read_ustar(&padding, &Limits::BOOTSTRAP).is_err());
        let mut checksum = image.clone(); checksum[0] ^= 1;
        assert!(read_ustar(&checksum, &Limits::BOOTSTRAP).is_err());
        let mut gnu = image.clone(); gnu[257..265].copy_from_slice(b"ustar  \0");
        let mut gnu_header: [u8; 512] = gnu[..512].try_into().unwrap(); seal_checksum(&mut gnu_header);
        gnu[..512].copy_from_slice(&gnu_header);
        assert!(read_ustar(&gnu, &Limits::BOOTSTRAP).is_err());
        let mut base256 = image.clone(); base256[124] = 0x80;
        let mut base256_header: [u8; 512] = base256[..512].try_into().unwrap(); seal_checksum(&mut base256_header);
        base256[..512].copy_from_slice(&base256_header);
        assert!(read_ustar(&base256, &Limits::BOOTSTRAP).is_err());
        let mut noisy_name = image; noisy_name[50] = b'x'; // byte after the name's NUL terminator
        let mut noisy_header: [u8; 512] = noisy_name[..512].try_into().unwrap(); seal_checksum(&mut noisy_header);
        noisy_name[..512].copy_from_slice(&noisy_header);
        assert!(read_ustar(&noisy_name, &Limits::BOOTSTRAP).is_err());
        assert!(read_ustar(&[], &Limits::BOOTSTRAP).is_err());
        assert!(read_ustar(&[0u8; 1024], &Limits::BOOTSTRAP).is_err());
    }

    #[test]
    fn ustar_counts_every_header_toward_the_entry_cap() {
        let base = inbox_entries();
        let exact = Limits { max_entries: base.len(), ..Limits::BOOTSTRAP };
        assert!(read_ustar(&tar(&base), &exact).is_ok());
        assert!(read_ustar(&tar(&base), &Limits { max_entries: base.len() - 1, ..exact }).is_err());
        // A header of a refused type still consumes the budget first: with the cap
        // already reached, the extra header is refused by the count itself.
        let mut out = Vec::new();
        for (name, data) in &base { append(&mut out, name, data); }
        let record = pax_record("path", "artifacts/a");
        append_raw(&mut out, header("PaxHeader/a", record.len(), b'x'), &record);
        append(&mut out, "artifacts/a", b"a");
        let image = finish(out);
        assert!(read_ustar(&image, &exact).is_err());
        assert!(read_ustar(&image, &Limits { max_entries: base.len() + 2, ..exact }).is_err());
        // The production cap: MAX_ARTIFACTS artifact headers plus the four metadata members.
        let mut out = Vec::new();
        for (name, data) in base.iter().take(4) { append(&mut out, name, data); }
        for index in 0..MAX_ARTIFACTS { append(&mut out, &format!("artifacts/a{index}"), b"x"); }
        let full = out.clone();
        assert_eq!(read_ustar(&finish(full), &Limits::BOOTSTRAP).unwrap().artifacts.len(), MAX_ARTIFACTS);
        append(&mut out, "artifacts/one-more", b"x");
        assert!(read_ustar(&finish(out), &Limits::BOOTSTRAP).is_err());
    }

    /// A real release signature (official key) over b"FAKE_BINARY", trusted comment "duduclaw test".
    pub(crate) const OFFICIAL_SIGNATURE: &str = "untrusted comment: signature from minisign secret key\nRUTh5pOpk0YmdButYZG9tlcekzhpZGpz83+E62dF1rtB5NtAoxXBenf0jZrluKejtL1R5cHtL60+75EzvH/EorwGHcc2aZlwkwQ=\ntrusted comment: duduclaw test\nmRGk2RUiXNVr9zzLXu6BI9+0URr0xlBwS3rMMXD3smkK7rcMrajd/tMz7jhWxQkiPzmWe4pdxFiIG1Vx2JdFAQ==\n";

    #[test]
    fn minisign_signature_text_is_strict() {
        let verify = |text: &str| verify_legacy_signature(b"FAKE_BINARY", text, LEGACY_RELEASE_MINISIGN_KEY, "duduclaw test");
        verify(OFFICIAL_SIGNATURE).unwrap();
        strict_signature_text(OFFICIAL_SIGNATURE).unwrap();
        let lines: Vec<&str> = OFFICIAL_SIGNATURE.lines().collect();
        assert_eq!(lines.len(), 4);
        let crlf = OFFICIAL_SIGNATURE.replace('\n', "\r\n");
        let joined = |parts: &[&str]| parts.iter().map(|line| format!("{line}\n")).collect::<String>();
        for bad in [
            format!("{OFFICIAL_SIGNATURE}trailing line\n"),
            format!("{OFFICIAL_SIGNATURE}\n"),
            format!("{OFFICIAL_SIGNATURE}x"),
            OFFICIAL_SIGNATURE.trim_end_matches('\n').to_owned(),
            crlf,
            joined(&[lines[0], "", lines[1], lines[2], lines[3]]),
            joined(&[lines[0], lines[1], lines[2]]),
            joined(&[lines[1], lines[2], lines[3]]),
            joined(&["comment: x", lines[1], lines[2], lines[3]]),
            joined(&[lines[0], lines[1], "comment: duduclaw test", lines[3]]),
            joined(&[lines[0], &format!("{} ", lines[1]), lines[2], lines[3]]),
            joined(&[lines[0], lines[1], &format!("{}\t", lines[2]), lines[3]]),
            joined(&[lines[0], lines[1], lines[2], &format!("{}\r", lines[3])]),
            String::new(),
        ] {
            assert!(strict_signature_text(&bad).is_err(), "{bad:?}");
            assert!(verify(&bad).is_err(), "{bad:?}");
        }
    }
}
