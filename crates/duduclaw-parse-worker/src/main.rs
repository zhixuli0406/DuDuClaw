#[cfg(target_os = "linux")]
fn serve() -> Result<(), duduclaw_parse_worker::worker::ParseError> {
    use std::os::fd::AsFd;
    use std::time::{Duration, Instant};
    use duduclaw_parse_worker::{policy, transport, worker::*, sandbox, parsers};
    use nix::sys::socket::{getsockopt, sockopt, SockType};
    // Do not expose even the pre-parse receive phase through same-uid procfs.
    nix::sys::prctl::set_dumpable(false).map_err(|_| ParseError::Unavailable)?;
    let stdin = std::io::stdin();
    let socket = stdin.as_fd();
    if getsockopt(&socket, sockopt::SockType).map_err(|_| ParseError::Unavailable)? != SockType::SeqPacket {
        return Err(ParseError::Unavailable);
    }
    // systemd gives fd 0; discard accidental inherited capabilities before input.
    #[allow(unsafe_code)]
    // SAFETY: binary is single-threaded, does not own other Rust fd wrappers.
    if unsafe { libc::syscall(libc::SYS_close_range, 3u32, u32::MAX, 0u32) } != 0 {
        return Err(ParseError::Unavailable);
    }
    // Parser diagnostics never enter stderr/journal, even on a caught panic.
    std::panic::set_hook(Box::new(|_| {}));
    let deadline = Instant::now() + Duration::from_millis(policy::MAX_WALL_TIME_MS as u64);
    let (payload, mut files) = transport::receive(&socket, 24, deadline)?;
    let request = ParseRequest::decode_payload(&payload).map_err(|_| ParseError::InvalidContent)?;
    let result = policy::validate(&request).and_then(|_| transport::validate_inputs(&request, &mut files))
        .and_then(|_| sandbox::apply(&request.limits, request.outputs.as_slice().iter().map(|o| o.max_bytes.get()).max().unwrap_or(0)));
    let response = match result {
        Ok(()) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (inputs, outputs) = files.split_at_mut(request.inputs.as_slice().len());
            let special = duduclaw_parse_worker::archive_adapter::parse(&request, inputs, outputs)
                .or_else(|| duduclaw_parse_worker::media_adapters::parse(&request, inputs, outputs));
            if let Some(result) = special {
                return ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
                    result: match result.and_then(|items| duduclaw_isolation::bounded::List::new(items)
                        .map_err(|_| ParseError::LimitExceeded)) {
                        Ok(items) => ParseResult::Complete { items },
                        Err(code) => ParseResult::Failed { code },
                    } };
            }
            parsers::parse(&request, inputs)
        }))
            .unwrap_or(ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
                result: ParseResult::Failed { code: ParseError::InvalidContent } }),
        Err(code) => ParseResponse { version: WORKER_PROTOCOL_VERSION, request_id: request.request_id,
            result: ParseResult::Failed { code } },
    };
    drop(files); // Release every writable output before the client seals it.
    let payload = duduclaw_parse_worker::response::encode(&response, &request)?;
    transport::send(&socket, &payload, &[], deadline)
}

fn main() {
    #[cfg(target_os = "linux")]
    let ok = serve().is_ok();
    #[cfg(not(target_os = "linux"))]
    let ok = false;
    if !ok { std::process::exit(1); }
}
