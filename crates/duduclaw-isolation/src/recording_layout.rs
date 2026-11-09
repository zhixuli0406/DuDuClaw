//! H12d: the one layout of a protected recording under the employee work root.
//!
//! The recording helper (`duduclaw-execution::recording_io::publish`) writes
//! here and the `skill_from_recording` reader (`mcp-boundary`
//! `ResourceRoots::read_recording_artifact`) opens exactly these relative
//! paths through root's work broker. Before H12d the reader looked for
//! `<home>/recordings/<id>/{meta.json,done.json,session.har}` on the host,
//! which the protected helper never produces. Both sides must use this module.

/// Directory under the employee work root (`/work`) holding recordings.
pub const RECORDINGS_DIR: &str = "recordings";
/// Artifacts of a completed protected recording, in manifest order.
pub const ARTIFACTS: [&str; 3] = ["trace.zip", "actions.json", "network.json"];
/// Largest single artifact the helper publishes (and the broker returns).
pub const MAX_ARTIFACT_BYTES: u64 = 16 * 1024 * 1024;

/// A protected recording id: the 32 lowercase hex characters of its job handle.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// `recordings/<id>/<artifact>` relative to the work root, or `None` for an
/// invalid id or an artifact name outside [`ARTIFACTS`].
pub fn artifact_path(id: &str, artifact: &str) -> Option<String> {
    (valid_id(id) && ARTIFACTS.contains(&artifact)).then(|| format!("{RECORDINGS_DIR}/{id}/{artifact}"))
}

/// SFR: the parse worker's single input for one recording. The host only
/// frames the three verified artifacts (it never parses them): the magic,
/// then for each of [`ARTIFACTS`] in order a little-endian `u32` length and
/// the bytes. The worker parses the frame and the content (`unbundle`).
pub const BUNDLE_MAGIC: [u8; 8] = *b"DDCREC1\n";
/// Largest framed input: magic + three length prefixes + three artifacts.
pub const MAX_BUNDLE_BYTES: u64 = BUNDLE_MAGIC.len() as u64 + 3 * (4 + MAX_ARTIFACT_BYTES);

/// Frame `parts` (in [`ARTIFACTS`] order). `None` for an empty or oversized part.
pub fn bundle(parts: [&[u8]; 3]) -> Option<Vec<u8>> {
    if parts.iter().any(|part| part.is_empty() || part.len() as u64 > MAX_ARTIFACT_BYTES) { return None; }
    let mut out = Vec::with_capacity(BUNDLE_MAGIC.len() + parts.iter().map(|part| 4 + part.len()).sum::<usize>());
    out.extend_from_slice(&BUNDLE_MAGIC);
    for part in parts {
        out.extend_from_slice(&u32::try_from(part.len()).ok()?.to_le_bytes());
        out.extend_from_slice(part);
    }
    Some(out)
}

/// Strict inverse of [`bundle`]: exact magic, three non-empty parts within
/// [`MAX_ARTIFACT_BYTES`], no trailing bytes. Anything else is `None`.
pub fn unbundle(bytes: &[u8]) -> Option<[&[u8]; 3]> {
    if bytes.len() as u64 > MAX_BUNDLE_BYTES { return None; }
    let mut rest = bytes.strip_prefix(&BUNDLE_MAGIC[..])?;
    let mut parts: [&[u8]; 3] = [&[]; 3];
    for part in &mut parts {
        let (length, tail) = rest.split_first_chunk::<4>()?;
        let length = u32::from_le_bytes(*length) as usize;
        if length == 0 || length as u64 > MAX_ARTIFACT_BYTES || length > tail.len() { return None; }
        let (body, tail) = tail.split_at(length);
        *part = body;
        rest = tail;
    }
    rest.is_empty().then_some(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recording_artifact_paths_are_fixed_and_relative() {
        let id = "ab".repeat(16);
        assert_eq!(artifact_path(&id, "actions.json").as_deref(), Some(format!("recordings/{id}/actions.json").as_str()));
        assert_eq!(artifact_path(&id, "network.json").as_deref(), Some(format!("recordings/{id}/network.json").as_str()));
        for bad in ["meta.json", "done.json", "session.har", "../trace.zip", "", "actions.json/"] {
            assert!(artifact_path(&id, bad).is_none(), "{bad}");
        }
        for bad in ["AB".repeat(16), "a".repeat(31), "../".repeat(11), String::new()] {
            assert!(artifact_path(&bad, "actions.json").is_none(), "{bad}");
        }
    }

    #[test]
    fn sfr_recording_bundle_frames_exactly_three_parts() {
        let framed = bundle([b"zip", b"{\"actions\":[]}", b"{}"]).unwrap();
        assert_eq!(unbundle(&framed), Some([&b"zip"[..], &b"{\"actions\":[]}"[..], &b"{}"[..]]));
        assert!(bundle([b"", b"a", b"b"]).is_none(), "empty parts are never framed");
        // Truncated, trailing, wrong magic, a zero or an overlong length: refused.
        assert!(unbundle(&framed[..framed.len() - 1]).is_none());
        let mut trailing = framed.clone(); trailing.push(0);
        assert!(unbundle(&trailing).is_none());
        let mut magic = framed.clone(); magic[0] = b'X';
        assert!(unbundle(&magic).is_none());
        let mut zero = framed.clone(); zero[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert!(unbundle(&zero).is_none());
        let mut long = framed; long[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(unbundle(&long).is_none());
    }
}
