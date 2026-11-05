// Compile the actual std-only installer policy without building gateway/CLI:
// rustc --edition=2021 --test scripts/tests/protected_layout_harness.rs -o /tmp/iso-layout-tests
#[path = "../../crates/duduclaw-cli/src/service/protected_linux_layout.rs"]
mod layout;
#[path = "../../crates/duduclaw-cli/src/service/protected_linux_lifecycle.rs"]
mod lifecycle;
