//! Bind a Unified Kernel Image to one specific root partition (H3d).
//!
//! ## Why this exists at all
//!
//! The A/B design (`commercial/docs/DESIGN-ab-update-rollback-2026-08.md`
//! §4.2/§4.3, option A) requires that a kernel and the root partition it
//! mounts retire *together*: sd-boot's boot counting can only retire an ESP
//! entry, and a kernel whose modules live on the other slot's root is a
//! broken boot. So every UKI carries `root=PARTUUID=<its own slot>` on its
//! kernel command line, and installing an update means installing the UKI
//! that names the slot the payload was written into.
//!
//! The original plan was to ship both slot variants pre-built from the
//! release host (`appliance/tools/uki-slots.py` derives them). **Measured
//! 2026-08-24, and it invalidates that plan for shipped payloads**: mkosi
//! seeds `systemd-repart` with a fresh random UUID per build, so two builds
//! of the same image have entirely different partition UUIDs —
//!
//! ```text
//! build A  root slot A = 5c058150-7f3d-4167-9241-3f83579b431f
//! build B  root slot A = e634b5f8-4956-4f9c-af12-471543a53375
//! ```
//!
//! A UKI carrying the *release host's* PARTUUID would therefore boot a
//! device into an initrd waiting forever for a partition that does not exist
//! on it — which is exactly the deliberate fault the T3 rollback test
//! injects. The binding must happen **on the device**, against the GPT that
//! is actually there, which is what this module does: the payload ships one
//! UKI *template* and staging rewrites its `root=PARTUUID=` to the
//! destination slot's real UUID.
//!
//! (The alternative — pinning `Seed=` in `mkosi.conf` so PARTUUIDs are
//! reproducible across builds — is recorded as a rejected option in the H3d
//! report: it would also work, but it cannot fix devices already flashed
//! from a random-seed image, and it makes the payload's correctness depend
//! on a build-config value nothing verifies at install time.)
//!
//! ## Why a byte rewrite instead of rebuilding the UKI
//!
//! Both UUIDs are exactly 36 ASCII characters, so the substitution changes
//! no section size, no PE header field and no file offset — it *cannot*
//! produce a structurally different image. Rebuilding a UKI on the appliance
//! would mean shipping `ukify`, a stub, and reproducing mkosi's exact
//! microcode/initrd assembly. Same reasoning `appliance/tools/uki-slots.py`
//! documents for its build-host derivation; this is that logic in Rust, on
//! the install side.
//!
//! ## Parsing discipline
//!
//! Every offset read here comes from a **downloaded artifact**. Signature
//! verification runs first (see [`crate::os_update`]), but this parser still
//! bounds-checks every field and returns `Err` rather than panicking on a
//! malformed image — a slice panic inside the gateway is a denial of service
//! on the one process that can install the fix.
//!
//! ## T4 status update (2026-09-02) — rewrite is now the fallback, not the norm
//!
//! `commercial/docs/DESIGN-os-trust-chain-2026-09.md`'s 2026-09-02 修正案
//! entry: once Secure Boot signs the whole UKI as one Authenticode PE, the
//! [`rewrite_root_partuuid`] byte-patch described above corrupts that
//! signature — a SB-enforcing firmware refuses to load the patched image.
//! The fix ships one pre-signed UKI *per slot* instead (root-B's own
//! PARTUUID had to become a build-time constant for that to be possible —
//! see `duduclaw-ab-partflags.bbclass`'s `DUDUCLAW_AB_ROOTB_PARTUUID`), and
//! [`crate::os_update`] now prefers *selecting* the already-correct variant
//! ([`verify_root_partuuid`]) over patching one. `rewrite_root_partuuid`
//! itself is unchanged and stays reachable: a release that still ships only
//! one UKI template (pre-T4, or a line that never adopts per-slot UKIs)
//! falls back to it, with a logged warning that SB enforcement will reject
//! the result.

/// PE section name holding the kernel command line of a UKI.
const CMDLINE_SECTION: &str = ".cmdline";

/// The token whose value we rewrite. Fixed width by construction: a
/// canonical UUID is always 36 characters.
const ROOT_PARTUUID_TOKEN: &str = "root=PARTUUID=";

/// Length of a canonical (hyphenated, lowercase) UUID string.
pub const UUID_TEXT_LEN: usize = 36;

/// Where a UKI's `.cmdline` section lives inside the file, plus its decoded
/// text (NUL-terminated in the section, so the text stops at the first NUL).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CmdlineSpan {
    pub offset: usize,
    pub size: usize,
    pub text: String,
}

fn read_u16(data: &[u8], at: usize) -> Result<u16, String> {
    data.get(at..at + 2)
        .map(|b| u16::from_le_bytes([b[0], b[1]]))
        .ok_or_else(|| format!("truncated PE image: no u16 at offset {at}"))
}

fn read_u32(data: &[u8], at: usize) -> Result<u32, String> {
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or_else(|| format!("truncated PE image: no u32 at offset {at}"))
}

/// Locate the `.cmdline` section of a UKI (a PE/COFF image).
///
/// Layout per the PE spec: `MZ` at 0, `e_lfanew` (u32) at 0x3C pointing at
/// the `PE\0\0` signature; `NumberOfSections` at +6, `SizeOfOptionalHeader`
/// at +20; the section table follows the optional header, 40 bytes per
/// entry, with `SizeOfRawData` at +16 and `PointerToRawData` at +20.
pub fn cmdline_span(data: &[u8]) -> Result<CmdlineSpan, String> {
    if data.get(..2) != Some(&b"MZ"[..]) {
        return Err("not a PE image (missing MZ signature)".to_string());
    }
    let pe_off = read_u32(data, 0x3C)? as usize;
    if data.get(pe_off..pe_off + 4) != Some(&b"PE\0\0"[..]) {
        return Err(format!(
            "not a PE image (no PE signature at e_lfanew=0x{pe_off:x})"
        ));
    }
    let n_sections = read_u16(data, pe_off + 6)? as usize;
    // A UKI has a handful of sections (.linux/.initrd/.cmdline/.osrel/...).
    // The cap refuses an absurd header before it costs a long loop.
    if n_sections == 0 || n_sections > 96 {
        return Err(format!("implausible PE section count: {n_sections}"));
    }
    let opt_size = read_u16(data, pe_off + 20)? as usize;
    let table = pe_off
        .checked_add(24)
        .and_then(|v| v.checked_add(opt_size))
        .ok_or("PE header offsets overflow")?;

    for i in 0..n_sections {
        let entry = table
            .checked_add(i * 40)
            .ok_or("PE section table offset overflow")?;
        let name_bytes = data
            .get(entry..entry + 8)
            .ok_or("truncated PE section table")?;
        let name = String::from_utf8_lossy(name_bytes);
        let name = name.trim_end_matches('\0');
        if name != CMDLINE_SECTION {
            continue;
        }
        let raw_size = read_u32(data, entry + 16)? as usize;
        let raw_ptr = read_u32(data, entry + 20)? as usize;
        let end = raw_ptr
            .checked_add(raw_size)
            .ok_or("PE section extent overflows")?;
        if end > data.len() {
            return Err(format!(
                "{CMDLINE_SECTION} claims bytes {raw_ptr}..{end} but the image is {} bytes",
                data.len()
            ));
        }
        // Section payloads are NUL-padded; the command line is the run
        // before the first NUL.
        let raw = &data[raw_ptr..end];
        let text_end = raw.iter().position(|b| *b == 0).unwrap_or(raw.len());
        let text = String::from_utf8_lossy(&raw[..text_end]).into_owned();
        return Ok(CmdlineSpan {
            offset: raw_ptr,
            size: raw_size,
            text,
        });
    }
    Err(format!("UKI has no {CMDLINE_SECTION} section"))
}

/// True when `s` is a canonical hyphenated UUID (8-4-4-4-12 lowercase or
/// uppercase hex). Deliberately strict: this value is written into a kernel
/// command line, and the whole point of the fixed width is that the rewrite
/// cannot change the file's layout.
pub fn is_uuid_text(s: &str) -> bool {
    if s.len() != UUID_TEXT_LEN {
        return false;
    }
    let groups = [8usize, 4, 4, 4, 12];
    let mut parts = s.split('-');
    for want in groups {
        match parts.next() {
            Some(p) if p.len() == want && p.bytes().all(|b| b.is_ascii_hexdigit()) => {}
            _ => return false,
        }
    }
    parts.next().is_none()
}

/// Extract a fixed-width value immediately following `token` in a kernel
/// command line, validated by `valid`.
///
/// [`root_partuuid`] (36-char PARTUUID) and [`cmdline_roothash`] (64-char
/// dm-verity root hash, VER-V wave) are both instances of the same shape —
/// `<token><fixed-width-hex-or-uuid>` — that systemd-boot and
/// systemd-veritysetup-generator both expect on a kernel command line, so
/// this is the one place that walks a token, slices a fixed run of
/// characters after it, and hands the result to a shape validator.
fn cmdline_field(
    cmdline: &str,
    token: &str,
    len: usize,
    valid: impl Fn(&str) -> bool,
) -> Result<String, String> {
    let idx = cmdline
        .find(token)
        .ok_or_else(|| format!("the UKI's kernel command line has no {token}"))?;
    let start = idx + token.len();
    let value: String = cmdline[start..].chars().take(len).collect();
    if !valid(&value) {
        return Err(format!("{token} value {value:?} is not valid"));
    }
    Ok(value)
}

/// Extract the `root=PARTUUID=<uuid>` value from a kernel command line.
pub fn root_partuuid(cmdline: &str) -> Result<String, String> {
    cmdline_field(cmdline, ROOT_PARTUUID_TOKEN, UUID_TEXT_LEN, is_uuid_text)
}

/// The dm-verity root-hash token on a kernel command line (VER-V wave —
/// `commercial/docs/DESIGN-os-trust-chain-2026-09.md` §3.2 P1's cmdline
/// shape, once a build line bakes it in per the 2026-09-02 依賴鏈補記
/// entry). Per `systemd-veritysetup-generator(8)`, this is the SHA-256 root
/// hash of the dm-verity hash tree: 64 lowercase hex characters, fixed
/// width exactly like [`ROOT_PARTUUID_TOKEN`]'s 36-character UUID.
pub const ROOTHASH_TOKEN: &str = "roothash=";

/// Length of a SHA-256 hex digest — dm-verity's default (and this project's
/// only supported) root-hash algorithm.
pub const ROOTHASH_TEXT_LEN: usize = 64;

/// True when `s` is 64 ASCII hex characters (any case) — the shape of a
/// dm-verity SHA-256 root hash.
pub fn is_roothash_text(s: &str) -> bool {
    s.len() == ROOTHASH_TEXT_LEN && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Extract the `roothash=<hex>` value from a kernel command line, if
/// present.
///
/// `crate::os_update`'s VER-V consistency check treats `Err` here as "this
/// UKI's cmdline shape has not adopted `roothash=` yet" — a legitimate,
/// non-terminal state (not every build line has landed §3.2 P1's cmdline
/// shape), never a security failure by itself. Only a value that IS
/// present but disagrees with the release's signed hash tree is.
pub fn cmdline_roothash(cmdline: &str) -> Result<String, String> {
    cmdline_field(cmdline, ROOTHASH_TOKEN, ROOTHASH_TEXT_LEN, is_roothash_text)
}

/// Rewrite the UKI's `root=PARTUUID=` in place so it boots `new_partuuid`.
///
/// Returns the UUID that was there before (the payload's build-host value),
/// which the caller logs as provenance. Fails closed and leaves `data`
/// untouched on every error path.
///
/// The substitution is confined to the `.cmdline` section's byte range, so
/// no other section can be touched even if the same 36 characters happen to
/// appear inside the compressed initrd.
pub fn rewrite_root_partuuid(data: &mut [u8], new_partuuid: &str) -> Result<String, String> {
    if !is_uuid_text(new_partuuid) {
        return Err(format!(
            "refusing to write {new_partuuid:?} into a kernel command line: not a canonical UUID"
        ));
    }
    let span = cmdline_span(data)?;
    let old = root_partuuid(&span.text)?;
    if old.eq_ignore_ascii_case(new_partuuid) {
        return Ok(old);
    }

    let needle = format!("{ROOT_PARTUUID_TOKEN}{old}");
    let section = &data[span.offset..span.offset + span.size];
    let hits: Vec<usize> = section
        .windows(needle.len())
        .enumerate()
        .filter(|(_, w)| *w == needle.as_bytes())
        .map(|(i, _)| i)
        .collect();
    if hits.len() != 1 {
        return Err(format!(
            "expected exactly one root=PARTUUID= in {CMDLINE_SECTION}, found {}",
            hits.len()
        ));
    }
    let value_at = span.offset + hits[0] + ROOT_PARTUUID_TOKEN.len();
    data[value_at..value_at + UUID_TEXT_LEN].copy_from_slice(new_partuuid.as_bytes());

    // Re-parse the artifact instead of trusting the write: a rewrite that
    // silently moved the section (impossible by construction, but this is
    // the boot path) must be caught here, not by a machine that will not
    // come back up.
    let after = cmdline_span(data)?;
    if after.offset != span.offset || after.size != span.size {
        return Err("the .cmdline section moved during rewrite".to_string());
    }
    let now = root_partuuid(&after.text)?;
    if !now.eq_ignore_ascii_case(new_partuuid) {
        return Err(format!(
            "post-rewrite verification failed: cmdline still boots {now}, expected {new_partuuid}"
        ));
    }
    Ok(old)
}

/// Verify — without mutating anything — that a UKI's baked
/// `root=PARTUUID=` already equals `want`.
///
/// This is the T4 selection primitive (see the module doc's "T4 status
/// update"): `crate::os_update` calls it once per candidate UKI variant in a
/// per-slot release to find the one that is already bound to the
/// destination slot, instead of rewriting bytes and breaking a Secure Boot
/// signature that covers the whole PE image. Fails closed the same way
/// [`rewrite_root_partuuid`] does: a parse error, a missing token, or a
/// mismatch are all `Err`, never a silent false — a caller must not be able
/// to mistake "could not tell" for "does not match" or vice versa without
/// looking at the error text either way, but a caller that only checks
/// `is_ok()` still gets the fail-closed answer for all three.
///
/// Returns the baked PARTUUID on success (case-insensitively equal to
/// `want`) so the caller can log it as provenance, mirroring
/// `rewrite_root_partuuid`'s return value.
pub fn verify_root_partuuid(data: &[u8], want: &str) -> Result<String, String> {
    if !is_uuid_text(want) {
        return Err(format!(
            "refusing to verify against {want:?}: not a canonical UUID"
        ));
    }
    let span = cmdline_span(data)?;
    let got = root_partuuid(&span.text)?;
    if !got.eq_ignore_ascii_case(want) {
        return Err(format!(
            "UKI is bound to {got}, expected {want}"
        ));
    }
    Ok(got)
}

/// Shared test fixtures — `#[cfg(test)] pub(crate)` rather than private to
/// this module's own `tests` submodule so `crate::os_update`'s tests can
/// build the same structurally-real synthetic UKIs when exercising the T4
/// selection path (`bind_uki_to_slot`), instead of maintaining a second,
/// possibly-drifted copy of this PE-header arithmetic.
#[cfg(test)]
pub(crate) mod test_support {
    /// Build a minimal but structurally real PE image with one `.cmdline`
    /// section, so the parser is exercised against actual header arithmetic
    /// rather than a mock.
    pub(crate) fn synth_uki(cmdline: &str) -> Vec<u8> {
        let pe_off = 0x80usize;
        let opt_size = 0xF0usize;
        let table = pe_off + 24 + opt_size;
        let section_data_at = table + 2 * 40 + 16;
        let mut data = vec![0u8; section_data_at + 512];
        data[0] = b'M';
        data[1] = b'Z';
        data[0x3C..0x40].copy_from_slice(&(pe_off as u32).to_le_bytes());
        data[pe_off..pe_off + 4].copy_from_slice(b"PE\0\0");
        data[pe_off + 6..pe_off + 8].copy_from_slice(&2u16.to_le_bytes());
        data[pe_off + 20..pe_off + 22].copy_from_slice(&(opt_size as u16).to_le_bytes());

        // Section 0: .osrel (a decoy, so "first section wins" bugs show up).
        let s0 = table;
        data[s0..s0 + 6].copy_from_slice(b".osrel");
        data[s0 + 16..s0 + 20].copy_from_slice(&16u32.to_le_bytes());
        data[s0 + 20..s0 + 24].copy_from_slice(&(section_data_at as u32).to_le_bytes());

        // Section 1: .cmdline
        let s1 = table + 40;
        data[s1..s1 + 8].copy_from_slice(b".cmdline");
        let cmd_at = section_data_at + 16;
        data[s1 + 16..s1 + 20].copy_from_slice(&256u32.to_le_bytes());
        data[s1 + 20..s1 + 24].copy_from_slice(&(cmd_at as u32).to_le_bytes());
        data[cmd_at..cmd_at + cmdline.len()].copy_from_slice(cmdline.as_bytes());
        data
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::synth_uki;
    use super::*;

    const SLOT_A: &str = "5c058150-7f3d-4167-9241-3f83579b431f";
    const SLOT_B: &str = "650a421b-6979-44d9-b580-917f384a8325";

    fn sample_cmdline(uuid: &str) -> String {
        format!("systemd.show_status=auto rw root=PARTUUID={uuid} console=tty0")
    }

    #[test]
    fn parses_cmdline_of_a_synthetic_uki() {
        let uki = synth_uki(&sample_cmdline(SLOT_A));
        let span = cmdline_span(&uki).unwrap();
        assert!(span.text.contains("root=PARTUUID="));
        assert_eq!(root_partuuid(&span.text).unwrap(), SLOT_A);
    }

    #[test]
    fn rewrite_binds_the_uki_to_the_other_slot_without_resizing() {
        let mut uki = synth_uki(&sample_cmdline(SLOT_A));
        let before_len = uki.len();
        let before_span = cmdline_span(&uki).unwrap();

        let old = rewrite_root_partuuid(&mut uki, SLOT_B).unwrap();
        assert_eq!(old, SLOT_A);
        assert_eq!(uki.len(), before_len, "a UKI rewrite must not resize");

        let after = cmdline_span(&uki).unwrap();
        assert_eq!(after.offset, before_span.offset);
        assert_eq!(after.size, before_span.size);
        assert_eq!(root_partuuid(&after.text).unwrap(), SLOT_B);
        // Everything outside the 36 rewritten characters is untouched.
        assert!(after.text.starts_with("systemd.show_status=auto rw "));
        assert!(after.text.ends_with(" console=tty0"));
    }

    #[test]
    fn rewrite_to_the_same_uuid_is_a_no_op() {
        let mut uki = synth_uki(&sample_cmdline(SLOT_A));
        let copy = uki.clone();
        assert_eq!(rewrite_root_partuuid(&mut uki, SLOT_A).unwrap(), SLOT_A);
        assert_eq!(uki, copy);
    }

    #[test]
    fn rewrite_refuses_a_non_uuid_target() {
        let mut uki = synth_uki(&sample_cmdline(SLOT_A));
        let copy = uki.clone();
        for bad in [
            "",
            "not-a-uuid",
            // Right length, wrong shape — the exact case a length-only
            // check would let through into a kernel command line.
            "5c058150 7f3d 4167 9241 3f83579b431fXX",
            "5c058150-7f3d-4167-9241-3f83579b431",
            "/dev/sda2 root=/dev/sda3 init=/bin/sh  ",
        ] {
            assert!(
                rewrite_root_partuuid(&mut uki, bad).is_err(),
                "must refuse {bad:?}"
            );
        }
        assert_eq!(uki, copy, "a refused rewrite must not touch the image");
    }

    #[test]
    fn parser_rejects_malformed_images_without_panicking() {
        for bad in [
            vec![],
            b"MZ".to_vec(),
            b"not a pe image at all, just some bytes".to_vec(),
            {
                // Valid MZ, e_lfanew points past the end.
                let mut v = vec![0u8; 64];
                v[0] = b'M';
                v[1] = b'Z';
                v[0x3C..0x40].copy_from_slice(&0xFFFF_FF00u32.to_le_bytes());
                v
            },
        ] {
            assert!(cmdline_span(&bad).is_err());
        }
    }

    #[test]
    fn section_extent_past_end_of_file_is_an_error_not_a_panic() {
        let mut uki = synth_uki(&sample_cmdline(SLOT_A));
        let pe_off = 0x80usize;
        let opt_size = 0xF0usize;
        let s1 = pe_off + 24 + opt_size + 40;
        // Claim a gigantic .cmdline that runs off the end of the file.
        uki[s1 + 16..s1 + 20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(cmdline_span(&uki).is_err());
    }

    #[test]
    fn uki_without_root_partuuid_is_refused() {
        let mut uki = synth_uki("systemd.show_status=auto rw root=/dev/vda2");
        assert!(rewrite_root_partuuid(&mut uki, SLOT_B).is_err());
    }

    #[test]
    fn uuid_text_validator_shape() {
        assert!(is_uuid_text(SLOT_A));
        assert!(is_uuid_text(&SLOT_A.to_uppercase()));
        assert!(!is_uuid_text(&SLOT_A[..35]));
        assert!(!is_uuid_text(&format!("{SLOT_A}0")));
        assert!(!is_uuid_text("gggggggg-7f3d-4167-9241-3f83579b431f"));
        assert!(!is_uuid_text("5c058150_7f3d_4167_9241_3f83579b431f0"));
    }

    // --- verify_root_partuuid (T4) ----------------------------------------

    #[test]
    fn verify_accepts_a_uki_already_bound_to_the_target() {
        let uki = synth_uki(&sample_cmdline(SLOT_B));
        assert_eq!(verify_root_partuuid(&uki, SLOT_B).unwrap(), SLOT_B);
        // Case-insensitive, matching rewrite_root_partuuid's own contract.
        assert_eq!(
            verify_root_partuuid(&uki, &SLOT_B.to_uppercase()).unwrap(),
            SLOT_B
        );
    }

    #[test]
    fn verify_rejects_a_uki_bound_to_a_different_slot() {
        let uki = synth_uki(&sample_cmdline(SLOT_A));
        let err = verify_root_partuuid(&uki, SLOT_B).unwrap_err();
        assert!(err.contains(SLOT_A), "error should name the actual value: {err}");
    }

    #[test]
    fn verify_never_mutates_the_image() {
        let uki = synth_uki(&sample_cmdline(SLOT_A));
        let before = uki.clone();
        let _ = verify_root_partuuid(&uki, SLOT_B);
        let _ = verify_root_partuuid(&uki, SLOT_A);
        assert_eq!(uki, before, "verification must be read-only");
    }

    #[test]
    fn verify_refuses_a_non_uuid_target() {
        let uki = synth_uki(&sample_cmdline(SLOT_A));
        for bad in ["", "not-a-uuid", "/dev/sda2 root=/dev/sda3 init=/bin/sh  "] {
            assert!(verify_root_partuuid(&uki, bad).is_err(), "must refuse {bad:?}");
        }
    }

    #[test]
    fn verify_fails_closed_on_a_malformed_image() {
        assert!(verify_root_partuuid(&[], SLOT_A).is_err());
        assert!(verify_root_partuuid(b"not a pe image", SLOT_A).is_err());
    }

    #[test]
    fn verify_fails_closed_when_the_uki_has_no_root_partuuid() {
        let uki = synth_uki("systemd.show_status=auto rw root=/dev/vda2");
        assert!(verify_root_partuuid(&uki, SLOT_A).is_err());
    }

    // --- cmdline_roothash (VER-V wave) --------------------------------------

    const ROOTHASH_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ROOTHASH_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    #[test]
    fn roothash_field_extracts_a_valid_value() {
        let cmdline = format!("rw root=PARTUUID={SLOT_A} roothash={ROOTHASH_A} console=tty0");
        assert_eq!(cmdline_roothash(&cmdline).unwrap(), ROOTHASH_A);
        // Case-insensitive shape, same convention as root_partuuid/is_uuid_text.
        let upper = format!(
            "rw roothash={} root=PARTUUID={SLOT_A}",
            ROOTHASH_A.to_uppercase()
        );
        assert_eq!(cmdline_roothash(&upper).unwrap(), ROOTHASH_A.to_uppercase());
    }

    #[test]
    fn roothash_field_is_absent_on_a_pre_verity_cmdline() {
        // Today's shipping cmdline shape (no roothash= token at all) must be
        // a plain Err — the caller's job is to treat that as "not adopted
        // yet", not this function's.
        assert!(cmdline_roothash(&sample_cmdline(SLOT_A)).is_err());
    }

    #[test]
    fn roothash_field_rejects_a_present_but_malformed_value() {
        for bad_cmdline in [
            format!("roothash={}", &ROOTHASH_A[..63]), // too short
            format!("roothash={}", "z".repeat(64)),    // not hex
            "roothash=".to_string(),                   // nothing follows
        ] {
            assert!(
                cmdline_roothash(&bad_cmdline).is_err(),
                "must reject {bad_cmdline:?}"
            );
        }
    }

    #[test]
    fn is_roothash_text_shape() {
        assert!(is_roothash_text(ROOTHASH_A));
        assert!(is_roothash_text(&ROOTHASH_A.to_uppercase()));
        assert!(!is_roothash_text(&ROOTHASH_A[..63]));
        assert!(!is_roothash_text(&format!("{ROOTHASH_A}0")));
        assert!(!is_roothash_text(&"z".repeat(64)));
        assert_ne!(
            ROOTHASH_A, ROOTHASH_B,
            "sanity: the two fixtures must differ"
        );
    }
}
