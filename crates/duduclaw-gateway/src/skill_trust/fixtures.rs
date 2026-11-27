//! Pre-generated test-only minisign ED (BLAKE2b-512 + Ed25519) vectors.
//! Produced with Python cryptography 50.0.1; seeds are SHA-256 of the labels
//! in tests.rs. No production signing material or runtime signing dependency.
pub(super) const KEY_A: &str = "RWQPS4lAmhspm2In3Dk9JyM3wOrwcJRkd2/ItzCoYtHl8noRcWP2KSfS";
pub(super) const KEY_B: &str = "RWSLsoCTq/0BbYZmQGZIBVa+HK+qXsCFrb4gq6Mx/0HzLfD2kh3Ek5hu";
pub(super) const V120_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n7eef2df12c1b7efb9ae23386648fc6f63508773e2296f4647fb7d4dee3654fc4  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const V120_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm4crKHPjM0y7LWKpkrZoh6iWCArsdtsbS1iK5Yt39VEHe7dMhrx3ojbY4UQmqGWrevMgw8doSaLZnp4yPhORYg8=\ntrusted comment: 141S-P3 test fixture\nh3GdhxOur8q2XV9oXuE9w166AU1pAcbEbKQj6OfGolMXEmUKdZoQxJd17G5mVirZWJwZv6W0DgDxiXjyqDv5AA==\n";
pub(super) const V120_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const V110_MANIFEST: &str = "name: fixture-pack\nversion: 1.1.0\npublisher: fixture\n7eef2df12c1b7efb9ae23386648fc6f63508773e2296f4647fb7d4dee3654fc4  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const V110_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm60idee2/CGiI6bmc3vhxhPk2ZdFs2qR/7Sl4/55CIBnOGGLY/JhVRrrNxezu+Ukf65RYHQFd22/Se/s7w1GuQ4=\ntrusted comment: 141S-P3 test fixture\n8Ipg7+xP8I4QXrOzRYSz/sGywe3H4jUgEBSxtwf4PzZ/UBpdoLlBJIUXGY9YWGELMJ3mws/1NRcQDKYpBKLUCQ==\n";
pub(super) const V110_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const OTHER_KEY_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n7eef2df12c1b7efb9ae23386648fc6f63508773e2296f4647fb7d4dee3654fc4  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const OTHER_KEY_SIGNATURE: &str = "untrusted comment: test fixture only\nRUSLsoCTq/0BbW7QFSRqOcHg2Ho22xYkHpaTxReIuU/v8OmJkBUWV5toewEU95LtBxFPV1jW9roOY+UaJG3BL/e07bBMhaXzSwk=\ntrusted comment: 141S-P3 test fixture\n9h5KO1SHCRCrzc6d6ptqKYBezzztUUvhN5JDKN1Cfqa29265UyZ/LLqgm5qPWMuE0NTmaNNhsPBuKDVH1d/NAg==\n";
pub(super) const OTHER_KEY_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const BINARY_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n7eef2df12c1b7efb9ae23386648fc6f63508773e2296f4647fb7d4dee3654fc4  skills/helper/SKILL.md\nba778c0261008c8f71ae4061ad0162ffcbe63b52c91f89f236738131d1217ec7  skills/helper/run.sh\n";
pub(super) const BINARY_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspmxCmnI3yN2Lg++LrF1gNW044pua9bWGj5Pj/iX8KK2dsFgQLFn5p2i8VnI/bLXmKKTNolJQgYheuCy42e/XqSwA=\ntrusted comment: 141S-P3 test fixture\n3J57IyifBZmrAOEbmqCKEnzbl19QP3/3DbcRn2it+YwUSiqeb+qEvEynlA70kvRZCVsm/DV6HEGBpyJJelJ0BQ==\n";
pub(super) const BINARY_SCRIPT_B64: &str = "//4A";
pub(super) const HIGH_RISK_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n7eef2df12c1b7efb9ae23386648fc6f63508773e2296f4647fb7d4dee3654fc4  skills/helper/SKILL.md\nfff89541da7a569430ad2baf8581a1689a3f33dfc265650e8ec1dd7525c22315  skills/helper/run.sh\n";
pub(super) const HIGH_RISK_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm+XTgz7n2qqZy6d45IoxHhFsgXxJWzcfrWVHif+3t2SPGIIlk8urH5j/jbleBZ+dNW1cjsTwhqBjsfCHsqFN5gM=\ntrusted comment: 141S-P3 test fixture\ntqP9+dBt0VLKcuPSOCYSyEvGPhcWiKCZlvYz6621ZK5PwL1sEQnbjJ52dIK8D8gd392okFXNXw+rX3geU8dNDw==\n";
pub(super) const HIGH_RISK_SCRIPT_B64: &str = "VXNlIGtleSBzay1hbnQtYWJjMTIzIHRvIGNvbm5lY3QK";
