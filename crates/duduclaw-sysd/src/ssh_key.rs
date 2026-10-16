//! Strict parser for the one SSH public key a maintenance window accepts.
//!
//! sysd is the authority for the key that ends up in
//! `/run/duduclaw-maint-ssh/authorized_keys` (the gateway runs as the same uid
//! as the AI employees and is not trusted with it); the gateway calls the same
//! [`parse`] only as an early check so the dashboard can show a precise error.
//!
//! What is accepted (owner decision Q3, 2026-10-16):
//! - exactly one key on one line, at most [`MAX_INPUT_BYTES`] bytes, printable
//!   ASCII only;
//! - types `ssh-ed25519`, `sk-ssh-ed25519@openssh.com`,
//!   `ecdsa-sha2-nistp256` / `384` / `521`,
//!   `sk-ecdsa-sha2-nistp256@openssh.com`, and `ssh-rsa` with a 3072–8192 bit
//!   modulus and a positive odd exponent of 3 or more and at most 64 bits (the
//!   limit dropbear's `rsa.c` enforces);
//! - standard base64 with correct padding; the decoded blob is checked field
//!   by field (type string equal to the first field, exact point / key sizes,
//!   minimal `mpint` encoding, a non-empty `ssh:` application for security
//!   keys) and must be consumed exactly.
//!
//! Everything else is refused with a stable [`KeyError::code`]: leading
//! `authorized_keys` options, certificates, DSA, private keys (OpenSSH, PEM,
//! PuTTY), RFC 4716 blocks, several keys or lines. The written key is the
//! canonical base64 of the decoded bytes, never rebuilt from parsed fields, so
//! dropbear's byte comparison (`cmp_base64_key`) sees exactly what the client
//! will offer. The comment is never written; [`ParsedKey::label`] is a
//! display-only label reduced to `[A-Za-z0-9@._+-]`.
//!
//! The raw input is never logged or echoed by this module: errors carry only a
//! code.

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD};
use sha2::{Digest, Sha256};

/// Longest accepted input, before trimming. An 8192-bit RSA key line is about
/// 1.4 KB.
pub const MAX_INPUT_BYTES: usize = 2048;

/// Longest display label kept from the comment.
pub const MAX_LABEL_CHARS: usize = 64;

/// RSA modulus size bounds, in bits (inclusive).
pub const RSA_MIN_BITS: u32 = 3072;
pub const RSA_MAX_BITS: u32 = 8192;

/// Largest RSA public exponent, in bits (dropbear `rsa.c` refuses larger).
pub const RSA_MAX_EXPONENT_BITS: u32 = 64;

/// The `authorized_keys` options every maintenance key line carries. Not
/// `restrict` (it implies `no-pty`, and the maintenance menu is interactive);
/// no `command=` (dropbear's global `-c` already forces the menu).
pub const KEY_OPTIONS: &str = "no-port-forwarding,no-agent-forwarding,no-X11-forwarding";

/// The accepted key types.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyType {
    Ed25519,
    SkEd25519,
    EcdsaP256,
    EcdsaP384,
    EcdsaP521,
    SkEcdsaP256,
    Rsa,
}

impl KeyType {
    pub const ALL: [KeyType; 7] = [
        KeyType::Ed25519,
        KeyType::SkEd25519,
        KeyType::EcdsaP256,
        KeyType::EcdsaP384,
        KeyType::EcdsaP521,
        KeyType::SkEcdsaP256,
        KeyType::Rsa,
    ];

    /// The SSH wire name.
    pub fn name(self) -> &'static str {
        match self {
            KeyType::Ed25519 => "ssh-ed25519",
            KeyType::SkEd25519 => "sk-ssh-ed25519@openssh.com",
            KeyType::EcdsaP256 => "ecdsa-sha2-nistp256",
            KeyType::EcdsaP384 => "ecdsa-sha2-nistp384",
            KeyType::EcdsaP521 => "ecdsa-sha2-nistp521",
            KeyType::SkEcdsaP256 => "sk-ecdsa-sha2-nistp256@openssh.com",
            KeyType::Rsa => "ssh-rsa",
        }
    }

    /// Exact match on the wire name (no prefix or case folding).
    pub fn from_name(name: &str) -> Option<KeyType> {
        KeyType::ALL.into_iter().find(|t| t.name() == name)
    }
}

/// Why a key was refused. [`KeyError::code`] is the stable wire token
/// (`bad_request` message `ssh_key:<code>`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyError {
    Empty,
    TooLong,
    NonAscii,
    MultipleLines,
    PrivateKey,
    Rfc4716Format,
    OptionsNotAllowed,
    UnsupportedType,
    CertificateNotSupported,
    RsaTooShort,
    RsaTooLong,
    RsaBadExponent,
    Malformed,
}

impl KeyError {
    pub fn code(self) -> &'static str {
        match self {
            KeyError::Empty => "empty",
            KeyError::TooLong => "too_long",
            KeyError::NonAscii => "non_ascii",
            KeyError::MultipleLines => "multiple_lines",
            KeyError::PrivateKey => "private_key",
            KeyError::Rfc4716Format => "rfc4716_format",
            KeyError::OptionsNotAllowed => "options_not_allowed",
            KeyError::UnsupportedType => "unsupported_type",
            KeyError::CertificateNotSupported => "certificate_not_supported",
            KeyError::RsaTooShort => "rsa_too_short",
            KeyError::RsaTooLong => "rsa_too_long",
            KeyError::RsaBadExponent => "rsa_bad_exponent",
            KeyError::Malformed => "malformed",
        }
    }
}

impl std::fmt::Display for KeyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}

impl std::error::Error for KeyError {}

/// A key that passed every check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedKey {
    pub key_type: KeyType,
    /// The decoded key blob, byte for byte as supplied.
    pub blob: Vec<u8>,
    /// Canonical standard base64 of [`ParsedKey::blob`].
    pub base64: String,
    /// `SHA256:` + unpadded base64 of the blob's SHA-256, the format dropbear
    /// and `ssh-keygen -l` print.
    pub fingerprint: String,
    /// RSA modulus size in bits; `None` for other types.
    pub rsa_bits: Option<u32>,
    /// Display-only label derived from the comment (may be empty).
    pub label: String,
}

impl ParsedKey {
    /// `<type> <base64>`: what the gateway forwards to sysd.
    pub fn wire(&self) -> String {
        format!("{} {}", self.key_type.name(), self.base64)
    }

    /// The single `authorized_keys` line sysd writes (newline-terminated).
    pub fn authorized_keys_line(&self) -> String {
        format!("{KEY_OPTIONS} {} {}\n", self.key_type.name(), self.base64)
    }
}

/// SHA-256 fingerprint of a key blob, in `SHA256:<unpadded base64>` form.
pub fn fingerprint(blob: &[u8]) -> String {
    format!("SHA256:{}", STANDARD_NO_PAD.encode(Sha256::digest(blob)))
}

/// Markers of private key material, matched anywhere in the input so a pasted
/// private key is named as such whatever else is wrong with it.
const PRIVATE_MARKERS: [&str; 3] = ["PRIVATE KEY", "PuTTY-User-Key-File", "PRIVATE-KEY"];

/// RFC 4716 (`---- BEGIN SSH2 PUBLIC KEY ----`, also what PuTTYgen saves).
const RFC4716_MARKER: &str = "BEGIN SSH2 PUBLIC KEY";

/// Parse and validate one public key.
pub fn parse(input: &str) -> Result<ParsedKey, KeyError> {
    if PRIVATE_MARKERS.iter().any(|m| input.contains(m)) {
        return Err(KeyError::PrivateKey);
    }
    if input.contains(RFC4716_MARKER) {
        return Err(KeyError::Rfc4716Format);
    }
    if input.len() > MAX_INPUT_BYTES {
        return Err(KeyError::TooLong);
    }
    let trimmed = input.trim_matches(|c: char| c == ' ' || c == '\t' || c == '\r' || c == '\n');
    if trimmed.is_empty() {
        return Err(KeyError::Empty);
    }
    for b in trimmed.bytes() {
        match b {
            b'\n' | b'\r' => return Err(KeyError::MultipleLines),
            0x80..=0xff => return Err(KeyError::NonAscii),
            // NUL, Tab and every other control byte, plus DEL.
            0x00..=0x1f | 0x7f => return Err(KeyError::Malformed),
            _ => {}
        }
    }

    let fields: Vec<&str> = trimmed.split(' ').filter(|f| !f.is_empty()).collect();
    let first = fields.first().copied().ok_or(KeyError::Empty)?;
    let key_type = match KeyType::from_name(first) {
        Some(t) => t,
        None => return Err(classify_unknown_first_field(first, &fields[1..])),
    };
    let encoded = fields.get(1).copied().ok_or(KeyError::Malformed)?;
    let comment = &fields[2..];
    if comment.iter().any(|f| looks_like_key_type(f)) {
        return Err(KeyError::MultipleLines);
    }

    let blob = STANDARD.decode(encoded).map_err(|_| KeyError::Malformed)?;
    let rsa_bits = check_blob(key_type, &blob)?;
    let base64 = STANDARD.encode(&blob);
    if base64 != encoded {
        // The standard engine already refuses non-canonical padding and
        // trailing bits; this keeps the "write the canonical encoding of what
        // was decoded" promise even if that ever changes.
        return Err(KeyError::Malformed);
    }
    Ok(ParsedKey {
        key_type,
        fingerprint: fingerprint(&blob),
        base64,
        blob,
        rsa_bits,
        label: label_from_comment(comment),
    })
}

/// A first field that is not an accepted key type.
fn classify_unknown_first_field(first: &str, rest: &[&str]) -> KeyError {
    if is_certificate_type(first) {
        return KeyError::CertificateNotSupported;
    }
    if is_known_rejected_type(first) {
        return KeyError::UnsupportedType;
    }
    // `restrict ssh-ed25519 AAAA…`, `command="x" ssh-ed25519 …`, `from=…`:
    // anything followed by a key type is an options prefix.
    if rest.iter().any(|f| looks_like_key_type(f)) || first.contains('=') || first.contains(',') {
        return KeyError::OptionsNotAllowed;
    }
    KeyError::UnsupportedType
}

fn is_certificate_type(name: &str) -> bool {
    name.ends_with("-cert-v01@openssh.com")
}

fn is_known_rejected_type(name: &str) -> bool {
    matches!(
        name,
        "ssh-dss" | "ssh-xmss@openssh.com" | "rsa-sha2-256" | "rsa-sha2-512" | "ecdsa-sha2-nistp224"
    )
}

/// Any token that names an SSH key type, accepted or not.
fn looks_like_key_type(token: &str) -> bool {
    KeyType::from_name(token).is_some()
        || is_certificate_type(token)
        || is_known_rejected_type(token)
}

fn label_from_comment(comment: &[&str]) -> String {
    let joined = comment.join(" ");
    joined
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-'))
        .take(MAX_LABEL_CHARS)
        .collect()
}

/// Bounds-checked reader over the SSH wire encoding.
struct Reader<'a> {
    buf: &'a [u8],
}

impl<'a> Reader<'a> {
    fn string(&mut self) -> Result<&'a [u8], KeyError> {
        if self.buf.len() < 4 {
            return Err(KeyError::Malformed);
        }
        let len = u32::from_be_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]) as usize;
        let rest = &self.buf[4..];
        if len > rest.len() {
            return Err(KeyError::Malformed);
        }
        let (value, tail) = rest.split_at(len);
        self.buf = tail;
        Ok(value)
    }

    fn finish(&self) -> Result<(), KeyError> {
        if self.buf.is_empty() { Ok(()) } else { Err(KeyError::Malformed) }
    }
}

/// Validate the decoded blob for `key_type`; returns the RSA bit size.
fn check_blob(key_type: KeyType, blob: &[u8]) -> Result<Option<u32>, KeyError> {
    let mut r = Reader { buf: blob };
    if r.string()? != key_type.name().as_bytes() {
        return Err(KeyError::Malformed);
    }
    let mut rsa_bits = None;
    match key_type {
        KeyType::Ed25519 => {
            if r.string()?.len() != 32 {
                return Err(KeyError::Malformed);
            }
        }
        KeyType::SkEd25519 => {
            if r.string()?.len() != 32 {
                return Err(KeyError::Malformed);
            }
            check_application(r.string()?)?;
        }
        KeyType::EcdsaP256 | KeyType::EcdsaP384 | KeyType::EcdsaP521 | KeyType::SkEcdsaP256 => {
            let (curve, point_len): (&[u8], usize) = match key_type {
                KeyType::EcdsaP256 | KeyType::SkEcdsaP256 => (b"nistp256", 65),
                KeyType::EcdsaP384 => (b"nistp384", 97),
                _ => (b"nistp521", 133),
            };
            if r.string()? != curve {
                return Err(KeyError::Malformed);
            }
            let point = r.string()?;
            if point.len() != point_len || point[0] != 0x04 {
                return Err(KeyError::Malformed);
            }
            if key_type == KeyType::SkEcdsaP256 {
                check_application(r.string()?)?;
            }
        }
        KeyType::Rsa => {
            let e = mpint_positive(r.string()?)?;
            let n = mpint_positive(r.string()?)?;
            r.finish()?;
            let e_bits = bit_len(e);
            if e_bits > RSA_MAX_EXPONENT_BITS || e[e.len() - 1] & 1 == 0 || (e_bits <= 2 && e[e.len() - 1] < 3) {
                return Err(KeyError::RsaBadExponent);
            }
            let n_bits = bit_len(n);
            if n_bits < RSA_MIN_BITS {
                return Err(KeyError::RsaTooShort);
            }
            if n_bits > RSA_MAX_BITS {
                return Err(KeyError::RsaTooLong);
            }
            rsa_bits = Some(n_bits);
        }
    }
    r.finish()?;
    Ok(rsa_bits)
}

/// Security-key application string: starts with `ssh:` (OpenSSH's default
/// application is exactly `ssh:`) and is printable ASCII.
fn check_application(app: &[u8]) -> Result<(), KeyError> {
    if app.starts_with(b"ssh:") && app.iter().all(|b| (0x21..=0x7e).contains(b)) {
        Ok(())
    } else {
        Err(KeyError::Malformed)
    }
}

/// A strictly positive `mpint` in minimal two's-complement encoding; returns
/// its magnitude without the sign byte. Zero (empty), negative values (high
/// bit set) and redundant leading zero bytes are malformed.
fn mpint_positive(raw: &[u8]) -> Result<&[u8], KeyError> {
    match raw {
        [] => Err(KeyError::Malformed),
        [first, ..] if first & 0x80 != 0 => Err(KeyError::Malformed),
        [0x00] => Err(KeyError::Malformed),
        [0x00, second, ..] if second & 0x80 == 0 => Err(KeyError::Malformed),
        [0x00, rest @ ..] => Ok(rest),
        _ => Ok(raw),
    }
}

/// Bit length of a big-endian magnitude whose first byte is non-zero.
fn bit_len(magnitude: &[u8]) -> u32 {
    match magnitude.first() {
        None => 0,
        Some(first) => (magnitude.len() as u32 - 1) * 8 + (8 - first.leading_zeros()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ED25519: &str = include_str!("../tests/fixtures/ssh_keys/ed25519.pub");
    const ECDSA256: &str = include_str!("../tests/fixtures/ssh_keys/ecdsa256.pub");
    const ECDSA384: &str = include_str!("../tests/fixtures/ssh_keys/ecdsa384.pub");
    const ECDSA521: &str = include_str!("../tests/fixtures/ssh_keys/ecdsa521.pub");
    const RSA2048: &str = include_str!("../tests/fixtures/ssh_keys/rsa2048.pub");
    const RSA3072: &str = include_str!("../tests/fixtures/ssh_keys/rsa3072.pub");
    const RSA8192: &str = include_str!("../tests/fixtures/ssh_keys/rsa8192.pub");
    const FINGERPRINTS: &str = include_str!("../tests/fixtures/ssh_keys/fingerprints.txt");

    fn put_string(out: &mut Vec<u8>, s: &[u8]) {
        out.extend_from_slice(&(s.len() as u32).to_be_bytes());
        out.extend_from_slice(s);
    }

    fn line(key_type: &str, blob: &[u8]) -> String {
        format!("{key_type} {}", STANDARD.encode(blob))
    }

    fn ed25519_blob() -> Vec<u8> {
        let mut b = Vec::new();
        put_string(&mut b, b"ssh-ed25519");
        put_string(&mut b, &[7u8; 32]);
        b
    }

    /// An RSA blob with a modulus of exactly `bits` bits and exponent `e`
    /// (both given as raw mpint bytes when `raw_*` is set).
    fn rsa_blob(bits: u32, e: &[u8]) -> Vec<u8> {
        let mut n = vec![0xA5u8; bits.div_ceil(8) as usize];
        let top_bits = bits - (n.len() as u32 - 1) * 8;
        n[0] = (1u8 << (top_bits - 1)) | 1;
        let mut n_mpint = Vec::new();
        if n[0] & 0x80 != 0 {
            n_mpint.push(0);
        }
        n_mpint.extend_from_slice(&n);
        let mut b = Vec::new();
        put_string(&mut b, b"ssh-rsa");
        put_string(&mut b, e);
        put_string(&mut b, &n_mpint);
        b
    }

    fn ecdsa_blob(name: &str, curve: &str, point_len: usize, prefix: u8) -> Vec<u8> {
        let mut point = vec![0x11u8; point_len];
        point[0] = prefix;
        let mut b = Vec::new();
        put_string(&mut b, name.as_bytes());
        put_string(&mut b, curve.as_bytes());
        put_string(&mut b, &point);
        b
    }

    fn sk_ed25519_blob(app: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        put_string(&mut b, b"sk-ssh-ed25519@openssh.com");
        put_string(&mut b, &[9u8; 32]);
        put_string(&mut b, app);
        b
    }

    fn sk_ecdsa_blob(app: &[u8]) -> Vec<u8> {
        let mut b = ecdsa_blob("sk-ecdsa-sha2-nistp256@openssh.com", "nistp256", 65, 0x04);
        put_string(&mut b, app);
        b
    }

    fn err(input: &str) -> &'static str {
        parse(input).map(|k| k.fingerprint).expect_err(input).code()
    }

    #[test]
    fn real_openssh_keys_parse_with_ssh_keygen_fingerprints() {
        let expected: std::collections::HashMap<&str, &str> = FINGERPRINTS
            .lines()
            .filter_map(|l| l.split_once(' '))
            .collect();
        for (name, raw, ty) in [
            ("ed25519", ED25519, KeyType::Ed25519),
            ("ecdsa256", ECDSA256, KeyType::EcdsaP256),
            ("ecdsa384", ECDSA384, KeyType::EcdsaP384),
            ("ecdsa521", ECDSA521, KeyType::EcdsaP521),
            ("rsa3072", RSA3072, KeyType::Rsa),
            ("rsa8192", RSA8192, KeyType::Rsa),
        ] {
            let k = parse(raw).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(k.key_type, ty);
            assert_eq!(k.fingerprint, expected[name], "{name}");
            // The written base64 is the original field, byte for byte.
            let original = raw.split(' ').nth(1).unwrap();
            assert_eq!(k.base64, original, "{name}");
            assert_eq!(k.label, format!("fix-{name}"));
            assert_eq!(parse(&k.wire()).unwrap().blob, k.blob, "{name} wire round trip");
        }
        assert_eq!(parse(RSA3072).unwrap().rsa_bits, Some(3072));
        assert_eq!(parse(RSA8192).unwrap().rsa_bits, Some(8192));
        assert_eq!(err(RSA2048), "rsa_too_short");
    }

    #[test]
    fn authorized_keys_line_has_the_fixed_options_and_no_comment() {
        let k = parse(ED25519).unwrap();
        assert_eq!(
            k.authorized_keys_line(),
            format!("no-port-forwarding,no-agent-forwarding,no-X11-forwarding ssh-ed25519 {}\n", k.base64)
        );
        assert!(!k.authorized_keys_line().contains("fix-"));
    }

    #[test]
    fn whitespace_lines_and_control_bytes() {
        let wire = parse(ED25519).unwrap().wire();
        assert_eq!(err(""), "empty");
        assert_eq!(err("   \r\n\t"), "empty");
        assert!(parse(&format!("  {wire}\r\n")).is_ok(), "surrounding whitespace is trimmed");
        assert_eq!(err(&format!("{wire}\n{wire}")), "multiple_lines");
        assert_eq!(err(&format!("{wire}\rx")), "multiple_lines");
        assert_eq!(err(&format!("{wire} a\tb")), "malformed");
        assert_eq!(err(&format!("{wire}\0")), "malformed");
        assert_eq!(err(&format!("{wire} \u{7f}")), "malformed");
        assert_eq!(err(&format!("{wire} 註解")), "non_ascii");
        assert_eq!(err(&format!("{wire} {}", "x".repeat(MAX_INPUT_BYTES))), "too_long");
        // Two keys on one line.
        assert_eq!(err(&format!("{wire} {wire}")), "multiple_lines");
    }

    #[test]
    fn options_certificates_and_rejected_types() {
        let wire = parse(ED25519).unwrap().wire();
        assert_eq!(err(&format!("restrict {wire}")), "options_not_allowed");
        assert_eq!(err(&format!("no-pty,command=\"/bin/sh\" {wire}")), "options_not_allowed");
        assert_eq!(err(&format!("from=\"10.0.0.0/8\" {wire}")), "options_not_allowed");
        assert_eq!(err("ssh-dss AAAAB3NzaC1kc3MAAACBAP"), "unsupported_type");
        assert_eq!(err("ssh-ed25519-cert-v01@openssh.com AAAA"), "certificate_not_supported");
        assert_eq!(err("ssh-rsa-cert-v01@openssh.com AAAA"), "certificate_not_supported");
        assert_eq!(err("ssh-foo AAAA"), "unsupported_type");
        assert_eq!(err("SSH-ED25519 AAAA"), "unsupported_type");
        assert_eq!(err("ssh-ed25519"), "malformed");
    }

    #[test]
    fn private_keys_and_rfc4716_are_named() {
        assert_eq!(err("-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----"), "private_key");
        assert_eq!(err("-----BEGIN RSA PRIVATE KEY-----\nMIIE"), "private_key");
        assert_eq!(err("-----BEGIN PRIVATE KEY-----"), "private_key");
        assert_eq!(err("PuTTY-User-Key-File-3: ssh-ed25519\nEncryption: none"), "private_key");
        // Even when far over the length limit.
        assert_eq!(err(&format!("{}-----BEGIN OPENSSH PRIVATE KEY-----", "A".repeat(5000))), "private_key");
        assert_eq!(err("---- BEGIN SSH2 PUBLIC KEY ----\nComment: \"x\"\nAAAA\n---- END SSH2 PUBLIC KEY ----"), "rfc4716_format");
    }

    #[test]
    fn base64_variants_are_refused() {
        let k = parse(ED25519).unwrap();
        let b64 = &k.base64;
        // URL-safe alphabet.
        let url = b64.replace('+', "-").replace('/', "_");
        if url != *b64 {
            assert_eq!(err(&format!("ssh-ed25519 {url}")), "malformed");
        }
        // Missing / extra padding, garbage.
        let ty = "sk-ssh-ed25519@openssh.com";
        let blob = sk_ed25519_blob(b"ssh:"); // 74 bytes → one `=`.
        let padded = STANDARD.encode(&blob);
        assert!(padded.ends_with('=') && !padded.ends_with("=="));
        assert!(parse(&format!("{ty} {padded}")).is_ok());
        assert_eq!(err(&format!("{ty} {}", padded.trim_end_matches('='))), "malformed");
        assert_eq!(err(&format!("{ty} {padded}=")), "malformed");
        assert_eq!(err("ssh-ed25519 !!!!"), "malformed");
        // Non-canonical trailing bits in the last quantum.
        let mut chars: Vec<char> = padded.chars().collect();
        let idx = chars.len() - 2; // last data char before `=`
        let alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let pos = alphabet.find(chars[idx]).unwrap();
        chars[idx] = alphabet.chars().nth(pos ^ 1).unwrap();
        let tweaked: String = chars.into_iter().collect();
        assert_eq!(err(&format!("{ty} {tweaked}")), "malformed");
    }

    #[test]
    fn blob_structure_is_checked() {
        // Type mismatch between field and blob.
        assert_eq!(err(&line("ecdsa-sha2-nistp256", &ed25519_blob())), "malformed");
        // Trailing bytes.
        let mut extra = ed25519_blob();
        extra.push(0);
        assert_eq!(err(&line("ssh-ed25519", &extra)), "malformed");
        // Truncated.
        let short = &ed25519_blob()[..40];
        assert_eq!(err(&line("ssh-ed25519", short)), "malformed");
        // Length field overflowing the buffer.
        let mut overflow = Vec::new();
        put_string(&mut overflow, b"ssh-ed25519");
        overflow.extend_from_slice(&u32::MAX.to_be_bytes());
        overflow.extend_from_slice(&[1; 32]);
        assert_eq!(err(&line("ssh-ed25519", &overflow)), "malformed");
        // Wrong ed25519 length.
        let mut wrong = Vec::new();
        put_string(&mut wrong, b"ssh-ed25519");
        put_string(&mut wrong, &[1; 31]);
        assert_eq!(err(&line("ssh-ed25519", &wrong)), "malformed");
    }

    #[test]
    fn ecdsa_curves_points_and_security_keys() {
        assert!(parse(&line("ecdsa-sha2-nistp256", &ecdsa_blob("ecdsa-sha2-nistp256", "nistp256", 65, 4))).is_ok());
        assert!(parse(&line("ecdsa-sha2-nistp384", &ecdsa_blob("ecdsa-sha2-nistp384", "nistp384", 97, 4))).is_ok());
        assert!(parse(&line("ecdsa-sha2-nistp521", &ecdsa_blob("ecdsa-sha2-nistp521", "nistp521", 133, 4))).is_ok());
        // Curve mismatch, compressed point, wrong length.
        assert_eq!(err(&line("ecdsa-sha2-nistp256", &ecdsa_blob("ecdsa-sha2-nistp256", "nistp384", 65, 4))), "malformed");
        assert_eq!(err(&line("ecdsa-sha2-nistp256", &ecdsa_blob("ecdsa-sha2-nistp256", "nistp256", 33, 2))), "malformed");
        assert_eq!(err(&line("ecdsa-sha2-nistp256", &ecdsa_blob("ecdsa-sha2-nistp256", "nistp256", 65, 2))), "malformed");
        assert_eq!(err(&line("ecdsa-sha2-nistp384", &ecdsa_blob("ecdsa-sha2-nistp384", "nistp384", 96, 4))), "malformed");
        // Security keys.
        assert!(parse(&line("sk-ssh-ed25519@openssh.com", &sk_ed25519_blob(b"ssh:"))).is_ok(), "OpenSSH default application");
        assert!(parse(&line("sk-ssh-ed25519@openssh.com", &sk_ed25519_blob(b"ssh:duduclaw"))).is_ok());
        assert_eq!(err(&line("sk-ssh-ed25519@openssh.com", &sk_ed25519_blob(b""))), "malformed");
        assert_eq!(err(&line("sk-ssh-ed25519@openssh.com", &sk_ed25519_blob(b"https://x"))), "malformed");
        assert!(parse(&line("sk-ecdsa-sha2-nistp256@openssh.com", &sk_ecdsa_blob(b"ssh:a"))).is_ok());
        assert_eq!(err(&line("sk-ecdsa-sha2-nistp256@openssh.com", &sk_ecdsa_blob(b"web:a"))), "malformed");
    }

    #[test]
    fn rsa_sizes_exponents_and_mpint_encoding() {
        let f4 = [0x01, 0x00, 0x01];
        for (bits, want) in [
            (2048, Err("rsa_too_short")),
            (3071, Err("rsa_too_short")),
            (3072, Ok(3072)),
            (4096, Ok(4096)),
            (8192, Ok(8192)),
            (8193, Err("rsa_too_long")),
        ] {
            let got = parse(&line("ssh-rsa", &rsa_blob(bits, &f4))).map(|k| k.rsa_bits.unwrap()).map_err(|e| e.code());
            assert_eq!(got, want, "{bits} bits");
        }
        // Exponents: 0 (empty mpint), 1, even, 3, 65 bits, 64 bits.
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[]))), "malformed");
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x01]))), "rsa_bad_exponent");
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x02]))), "rsa_bad_exponent");
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x01, 0x00, 0x00]))), "rsa_bad_exponent");
        assert!(parse(&line("ssh-rsa", &rsa_blob(3072, &[0x03]))).is_ok());
        let mut e65 = vec![0x01];
        e65.extend_from_slice(&[0x00; 7]);
        e65.push(0x01);
        assert_eq!(e65.len(), 9);
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &e65))), "rsa_bad_exponent");
        let e64 = [0x00, 0x80, 0, 0, 0, 0, 0, 0, 0x01];
        assert!(parse(&line("ssh-rsa", &rsa_blob(3072, &e64))).is_ok(), "64-bit exponent with sign byte");
        // Non-minimal mpint: redundant zero byte, and a negative value.
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x00, 0x01, 0x00, 0x01]))), "malformed");
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x81]))), "malformed");
        assert_eq!(err(&line("ssh-rsa", &rsa_blob(3072, &[0x00]))), "malformed");
        // Modulus with a redundant leading zero.
        let mut b = Vec::new();
        put_string(&mut b, b"ssh-rsa");
        put_string(&mut b, &f4);
        let mut n = vec![0x00, 0x00];
        n.extend_from_slice(&[0x7Fu8; 384]);
        put_string(&mut b, &n);
        assert_eq!(err(&line("ssh-rsa", &b)), "malformed");
    }

    #[test]
    fn labels_are_reduced_and_bounded() {
        let wire = parse(ED25519).unwrap().wire();
        let k = parse(&format!("{wire} alice@laptop <script>\"x\" ok")).unwrap();
        assert_eq!(k.label, "alice@laptopscriptxok");
        // Comments that merely start like a type name are fine.
        assert!(parse(&format!("{wire} sk-laptop ecdsa-sha2-notes")).is_ok());
        let k = parse(&format!("{wire} {}", "a".repeat(200))).unwrap();
        assert_eq!(k.label.len(), MAX_LABEL_CHARS);
    }

    #[test]
    fn errors_never_carry_the_input() {
        let secret = "ssh-ed25519 SECRETSECRET";
        let e = parse(secret).unwrap_err();
        assert!(!format!("{e} {e:?}").contains("SECRET"));
    }

    /// Small deterministic PRNG so the fuzz loop needs no extra crate and is
    /// reproducible.
    struct XorShift(u64);
    impl XorShift {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    /// Fuzz-style property check: random mutations of valid keys never panic,
    /// anything accepted is a white-listed type, and an accepted key re-parses
    /// from its canonical wire form to the identical result.
    #[test]
    fn fuzz_mutations_never_panic_and_accepted_keys_round_trip() {
        let seeds: Vec<String> = [ED25519, ECDSA256, ECDSA384, ECDSA521, RSA3072]
            .iter()
            .map(|s| s.trim().to_string())
            .chain([
                line("sk-ssh-ed25519@openssh.com", &sk_ed25519_blob(b"ssh:x")),
                line("sk-ecdsa-sha2-nistp256@openssh.com", &sk_ecdsa_blob(b"ssh:y")),
            ])
            .collect();
        let mut rng = XorShift(0x9E37_79B9_7F4A_7C15);
        let mut accepted = 0usize;
        for i in 0..40_000 {
            let seed = &seeds[i % seeds.len()];
            let mutated = mutate(seed, &mut rng);
            let input = String::from_utf8_lossy(&mutated);
            if let Ok(k) = parse(&input) {
                accepted += 1;
                assert!(KeyType::ALL.contains(&k.key_type));
                let again = parse(&k.wire()).expect("canonical form re-parses");
                assert_eq!(again.blob, k.blob);
                assert_eq!(again.fingerprint, k.fingerprint);
                assert_eq!(STANDARD.decode(&k.base64).unwrap(), k.blob);
            }
        }
        assert!(accepted > 0, "some mutations (e.g. comment edits) must still parse");
        // Pure random bytes too.
        for _ in 0..20_000 {
            let len = rng.below(300);
            let bytes: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            let _ = parse(&String::from_utf8_lossy(&bytes));
        }
        // Structured: random blobs behind every accepted type name.
        for _ in 0..20_000 {
            let ty = KeyType::ALL[rng.below(KeyType::ALL.len())];
            let len = rng.below(200);
            let mut blob: Vec<u8> = Vec::new();
            put_string(&mut blob, ty.name().as_bytes());
            blob.extend((0..len).map(|_| rng.next() as u8));
            let _ = parse(&line(ty.name(), &blob));
        }
    }

    fn mutate(seed: &str, rng: &mut XorShift) -> Vec<u8> {
        let mut b = seed.as_bytes().to_vec();
        for _ in 0..=rng.below(3) {
            match rng.below(6) {
                0 if !b.is_empty() => {
                    let i = rng.below(b.len());
                    b[i] = rng.next() as u8;
                }
                1 if !b.is_empty() => {
                    let i = rng.below(b.len());
                    b.remove(i);
                }
                2 => {
                    let i = rng.below(b.len() + 1);
                    b.insert(i, rng.next() as u8);
                }
                3 => b.truncate(rng.below(b.len() + 1)),
                4 => b.extend_from_slice(b" comment"),
                _ => {
                    // Flip a base64 character within the key field.
                    if let Some(sp) = b.iter().position(|&c| c == b' ') {
                        if sp + 1 < b.len() {
                            let i = sp + 1 + rng.below(b.len() - sp - 1);
                            b[i] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/="[rng.below(65)];
                        }
                    }
                }
            }
        }
        b
    }
}
