//! Reviewed executable allowlist. Update hashes explicitly when bundled code changes.
use std::io;

pub struct ScriptManifest {
    pub skill: &'static str,
    pub path: &'static str,
    pub sha256: &'static str,
}

/// Manifest version is tied to the binary release; each script also has its own digest.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
/// Resolver upload cutoff; direct pins alone do not lock transitive dependencies.
pub const EXCLUDE_NEWER: &str = "2026-03-01T00:00:00Z";
pub const MANIFEST: &[ScriptManifest] = &[
    ScriptManifest { skill: "docx", path: "scripts/create.py", sha256: "900daff48d0b073a36fc9eaefba36f827069a26a4a951d3694a23d9bec14cfae" },
    ScriptManifest { skill: "docx", path: "scripts/extract.py", sha256: "a12f991ba9f5beca033034f983a90113aa9a60a11fcba3e566d1ab464cf5cf24" },
    ScriptManifest { skill: "docx", path: "scripts/to_pdf.py", sha256: "a933df9a31e679dd1b5a70f352701fe0e79f2590f0bd86c576b5556b68d7102a" },
    ScriptManifest { skill: "pdf", path: "scripts/create.py", sha256: "66245ee964825ad1a058742d436ab1717f111bc820de680cd10bbbee76be63f4" },
    ScriptManifest { skill: "pdf", path: "scripts/extract.py", sha256: "a1dddc8f264e8ce4181dd7dc03c951c4362691fd62d2a72ffa7ebdfe6d720518" },
    ScriptManifest { skill: "pptx", path: "scripts/create.py", sha256: "0d401e56443a071f63a9d554189b3fc11cf423e0d3c63c486e63f5de3221c93c" },
    ScriptManifest { skill: "pptx", path: "scripts/extract.py", sha256: "927bfbc209e8a76a8d8e768531400cb54eaf2abcd33045e767cb569c932f4913" },
    ScriptManifest { skill: "pptx", path: "scripts/to_pdf.py", sha256: "a933df9a31e679dd1b5a70f352701fe0e79f2590f0bd86c576b5556b68d7102a" },
    ScriptManifest { skill: "xlsx", path: "scripts/create.py", sha256: "8db31e29148dcc6eaa9ba4b83ef861f21dea63afabacc988287f09889babe34b" },
    ScriptManifest { skill: "xlsx", path: "scripts/extract.py", sha256: "11d0bdc0fcc56c9bcee90d0b88a40ae10001a9c99dcee59c7972324dc0237e59" },
    ScriptManifest { skill: "xlsx", path: "scripts/to_pdf.py", sha256: "a933df9a31e679dd1b5a70f352701fe0e79f2590f0bd86c576b5556b68d7102a" },
];

pub fn verify_script(skill: &str, path: &str, bytes: &[u8]) -> io::Result<()> {
    let entry = MANIFEST.iter().find(|entry| entry.skill == skill && entry.path == path)
        .ok_or_else(|| io::Error::other("Script not found in platform manifest"))?;
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    let actual: String = digest.as_ref().iter().map(|b| format!("{b:02x}")).collect();
    if actual != entry.sha256 {
        return Err(io::Error::other("Office script manifest hash mismatch"));
    }
    Ok(())
}

pub fn embedded_script(skill: &str, stem: &str) -> io::Result<&'static [u8]> {
    let path = format!("scripts/{stem}.py");
    let bytes = crate::builtin_skills::BUILTIN_SKILL_FILES.iter()
        .find(|(name, _)| *name == skill)
        .and_then(|(_, files)| files.iter().find(|(name, _)| *name == path))
        .map(|(_, body)| body.as_bytes())
        .ok_or_else(|| io::Error::other("Script not found in platform manifest"))?;
    verify_script(skill, &path, bytes)?;
    Ok(bytes)
}
