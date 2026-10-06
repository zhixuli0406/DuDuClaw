//! CE binary entry point — delegates to `duduclaw_cli::entry_point_blocking`,
//! which runs the CLI on a thread with a 32 MiB stack (the OS main thread is
//! 1 MiB on Windows and overflowed inside `mcp-server`).

fn main() {
    duduclaw_cli::entry_point_blocking();
}
