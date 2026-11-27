//! Pre-generated test-only minisign ED (BLAKE2b-512 + Ed25519) vectors.
//! Produced with Python cryptography; seeds are SHA-256 of the labels
//! in tests.rs. No production signing material or runtime signing dependency.
pub(super) const KEY_A: &str = "RWQPS4lAmhspm2In3Dk9JyM3wOrwcJRkd2/ItzCoYtHl8noRcWP2KSfS";
pub(super) const KEY_B: &str = "RWSLsoCTq/0BbYZmQGZIBVa+HK+qXsCFrb4gq6Mx/0HzLfD2kh3Ek5hu";
pub(super) const V120_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n3b0cfb7da11beee87afb24a3628fe5027259a77c1e68201605a478f4956b8ee1  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const V120_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm+hcTNmgmGbIz8X2O00cdTZFObAZLF3zMJt4rnmv2GxBT8wGOL3MD3P79suHjTvMzEF3LCCFqXf+Kq/HryVPUQc=\ntrusted comment: 141S-P3 test fixture\nHRHIPi6LdscCLM2x82RQEHCLVGx45RSWTRQC8UR8RH+U2V5qlpjQwUlbnI2MtOetsQY+S/p8R4GvBJWCDpnZAQ==\n";
pub(super) const V120_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const V110_MANIFEST: &str = "name: fixture-pack\nversion: 1.1.0\npublisher: fixture\n3b0cfb7da11beee87afb24a3628fe5027259a77c1e68201605a478f4956b8ee1  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const V110_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspmy9s9ry4+r+dVU1gq585EM44IjgpRaXRllyHY+Z6Wzl+g+Ckfal17zNiMPK2cGJO0qvTYGhyKccgMOEE9aRaEAs=\ntrusted comment: 141S-P3 test fixture\n9owS7KU/3YdWqNcfDgG8IYwWZq3VSycrfnwz9Wfdq16Bu3D+VXJg3X7YmoQHGERhnHB1xXP0Po0P2WMlPweXAw==\n";
pub(super) const V110_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const OTHER_KEY_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n3b0cfb7da11beee87afb24a3628fe5027259a77c1e68201605a478f4956b8ee1  skills/helper/SKILL.md\nf869aed15d808e5e860a3202973185b70255b03ebd936437b381aa3eed6c18c5  skills/helper/run.sh\n";
pub(super) const OTHER_KEY_SIGNATURE: &str = "untrusted comment: test fixture only\nRUSLsoCTq/0BbUtQw6M4KHF2VEHLzqSA80aDLcFZkG9xFXTVOO+mDaSpg90So3AGCiXR7lAudkqK+zY6tgvdtrv2i8MdcKLwJww=\ntrusted comment: 141S-P3 test fixture\nPWumsEy5rQsayWFClQt54Z9oLnWC9ZXYpKr5ZP18qn08uefxdaFXyRNcv1j8dpMlBNCP4OF31MUokUBpsytBBA==\n";
pub(super) const OTHER_KEY_SCRIPT_B64: &str = "IyEvYmluL3NoCnByaW50ZiAnaGVsbG9cbicK";
pub(super) const BINARY_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n3b0cfb7da11beee87afb24a3628fe5027259a77c1e68201605a478f4956b8ee1  skills/helper/SKILL.md\nba778c0261008c8f71ae4061ad0162ffcbe63b52c91f89f236738131d1217ec7  skills/helper/run.sh\n";
pub(super) const BINARY_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm/jRqIHd5oOFNiKQAoRFrtaGbpyhxUPQJZUxccAIFPE2FCx4rrYa/K1shP9yobGi/8siC7l9gvs/9sZ4pq3FqQ0=\ntrusted comment: 141S-P3 test fixture\nDPhfQKNqFWI3Q0tZl35HOyu1QKluXEsEFjANI/AlK1PqcfzUiguWTAtNEQGK5+fkLTOocp2CWaJyMbzIDC+JBA==\n";
pub(super) const BINARY_SCRIPT_B64: &str = "//4A";
pub(super) const HIGH_RISK_MANIFEST: &str = "name: fixture-pack\nversion: 1.2.0\npublisher: fixture\n3b0cfb7da11beee87afb24a3628fe5027259a77c1e68201605a478f4956b8ee1  skills/helper/SKILL.md\nfff89541da7a569430ad2baf8581a1689a3f33dfc265650e8ec1dd7525c22315  skills/helper/run.sh\n";
pub(super) const HIGH_RISK_SIGNATURE: &str = "untrusted comment: test fixture only\nRUQPS4lAmhspm9i0nvcp40M1ALhLAQm2isSfUlvVWVMB9l7P9ksWYpoTSAKSb2gI3S7WCuIpJypwTfRjdUckZPJQ04Tk+XvNyAw=\ntrusted comment: 141S-P3 test fixture\n1cZ2A4jEmwy50YPQul8LamT4YBlchDAVHQgx7mJ7DkArvFaUDvJvzgsX8r5VjbgQ+AA8ZC18XXcKT9OQjM18BQ==\n";
pub(super) const HIGH_RISK_SCRIPT_B64: &str = "VXNlIGtleSBzay1hbnQtYWJjMTIzIHRvIGNvbm5lY3QK";
pub(super) const SIGNED_SOURCE: &str = "---\nname: helper\ndescription: Help with daily notes.\nlabel: 記事助手\n---\n# Helper\nHelp with daily notes.\n";
