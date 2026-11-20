use duduclaw_isolation::ids::RunId;
use duduclaw_spawnd::systemd::linux::Systemd;
use std::{io, time::{Duration, Instant}};

// Integration tests cannot use the library's cfg(test) helpers.
pub fn cleanup_run(systemd: &Systemd, run: RunId) -> io::Result<()> {
    systemd.kill_run(run)?;
    if !systemd.list_runs()?.contains(&run) { return Ok(()); }
    let unit = systemd.inspect_run(run)?;
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        if systemd.cleanup(&unit)? == (true, true) { break; }
        if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
        std::thread::sleep(Duration::from_millis(25));
    }
    systemd.release_reference(&unit)?;
    while systemd.list_runs()?.contains(&run) {
        if Instant::now() >= until { return Err(io::ErrorKind::TimedOut.into()); }
        std::thread::sleep(Duration::from_millis(25));
    }
    Ok(())
}
