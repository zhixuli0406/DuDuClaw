//! CI-only probe (feature `marker-probe`, never packaged): prints the licence
//! build identity exactly like the hidden `duduclaw print-license-issuer`.
//! `.github/workflows/linux-release-smoke.yml` builds it with the release
//! profile, normally and with `--cfg duduclaw_test_root`, and checks that
//! `scripts/refuse-test-license-build.py` refuses only the test-root build.

fn main() {
    println!("{}", duduclaw_gateway::license_runtime::license_build_identity());
}
