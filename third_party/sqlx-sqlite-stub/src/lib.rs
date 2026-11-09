//! Stub replacement for `sqlx-sqlite` 0.8.6.
//!
//! Why this exists (CVE-2026-119375 / RUSTSEC-2026-0459): `rusqlite` 0.41
//! bundles the patched SQLite 3.54 via `libsqlite3-sys` 0.39. sqlx's SQLite
//! driver is not used by DuDuClaw — `duduclaw-db` serves SQLite through
//! `rusqlite` — and `sqlx-sqlite` 0.8.x / 0.9.0 pin `libsqlite3-sys` < 0.38,
//! which bundles a vulnerable SQLite. Both crates declare `links = "sqlite3"`,
//! and Cargo resolves optional dependencies even when their feature is off, so
//! the real `sqlx-sqlite` made the lockfile unresolvable. The workspace
//! `[patch.crates-io]` swaps in this dependency-free stub instead.

compile_error!(
    "sqlx-sqlite is stubbed out; duduclaw-db's SQLite source uses rusqlite. Do not enable sqlx's `sqlite` feature."
);
