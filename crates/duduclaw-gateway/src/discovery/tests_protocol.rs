//! Protocol tests over in-memory pipes: a real policy driven across the
//! line protocol must replay exactly like the in-process baseline, and each
//! host-side hard rule must be reported distinctly.

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::sync::mpsc::{Receiver, Sender, channel};

use super::policy::{
    BaselineParallelRefine, ExplorationPolicy, GridContext, PolicyConfig, Question,
};
use super::protocol::{
    ClientMessage, EndReason, HostInit, HostResponse, InitBody, ProtocolViolation, ServeError,
    ServeLimits, request_plan_grid, serve_question,
};
use super::replay::{
    CellMeta, IllegalBatchReason, MetaError, Observation, ProbeError, Replay, TerminationReason,
};
use super::tests::full_grid;

struct ChanReader {
    rx: Receiver<Vec<u8>>,
    buf: Vec<u8>,
    pos: usize,
}

impl Read for ChanReader {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match self.rx.recv() {
                Ok(chunk) => {
                    self.buf = chunk;
                    self.pos = 0;
                }
                Err(_) => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

struct ChanWriter(Sender<Vec<u8>>);

impl Write for ChanWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0
            .send(data.to_vec())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn pipe() -> (ChanWriter, BufReader<ChanReader>) {
    let (tx, rx) = channel();
    (
        ChanWriter(tx),
        BufReader::new(ChanReader {
            rx,
            buf: Vec::new(),
            pos: 0,
        }),
    )
}

/// Client-side `Question` speaking the line protocol (test-only shim).
struct RemoteQuestion {
    reader: BufReader<ChanReader>,
    writer: ChanWriter,
    baseline: f64,
    mp: u32,
}

impl RemoteQuestion {
    fn connect(mut reader: BufReader<ChanReader>, writer: ChanWriter) -> Self {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        let HostInit::Init(InitBody::Solve {
            max_parallelism,
            baseline_score,
            ..
        }) = serde_json::from_str(&line).unwrap()
        else {
            panic!("expected solve init, got {line}");
        };
        Self {
            reader,
            writer,
            baseline: baseline_score,
            mp: max_parallelism,
        }
    }

    fn send(&mut self, msg: &ClientMessage) {
        let mut bytes = serde_json::to_vec(msg).unwrap();
        bytes.push(b'\n');
        self.writer.write_all(&bytes).unwrap();
    }

    fn call(&mut self, msg: &ClientMessage) -> HostResponse {
        self.send(msg);
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    fn cells(&mut self, msg: ClientMessage) -> Vec<String> {
        match self.call(&msg) {
            HostResponse::Cells { cells, .. } => cells,
            other => panic!("unexpected {other:?}"),
        }
    }
}

impl Question for RemoteQuestion {
    fn observed(&mut self) -> BTreeMap<String, Observation> {
        match self.call(&ClientMessage::Observed) {
            HostResponse::Observed { observed, .. } => observed,
            other => panic!("unexpected {other:?}"),
        }
    }
    fn legal_actions(&mut self) -> Vec<String> {
        self.cells(ClientMessage::LegalActions)
    }
    fn legal_roots(&mut self) -> Vec<String> {
        self.cells(ClientMessage::LegalRoots)
    }
    fn opened_branches(&mut self) -> Vec<u32> {
        match self.call(&ClientMessage::OpenedBranches) {
            HostResponse::Branches { branches, .. } => branches,
            other => panic!("unexpected {other:?}"),
        }
    }
    fn meta(&mut self, cell_id: &str) -> Result<CellMeta, MetaError> {
        match self.call(&ClientMessage::Meta {
            cell_id: cell_id.to_string(),
        }) {
            HostResponse::Meta { meta, .. } => Ok(meta),
            _ => Err(MetaError(cell_id.to_string())),
        }
    }
    fn probe_batch(&mut self, cells: &[String]) -> Result<Vec<Observation>, ProbeError> {
        match self.call(&ClientMessage::ProbeBatch {
            cells: cells.to_vec(),
        }) {
            HostResponse::Observations { observations, .. } => Ok(observations),
            HostResponse::Error { error, .. } if error == "terminated" => {
                Err(ProbeError::Terminated(TerminationReason::K2Reached))
            }
            HostResponse::Error { detail, .. } => Err(ProbeError::IllegalBatch(
                IllegalBatchReason::NotLegal(detail),
            )),
            other => panic!("unexpected {other:?}"),
        }
    }
    fn baseline_score(&self) -> f64 {
        self.baseline
    }
    fn max_parallelism(&self) -> u32 {
        self.mp
    }
}

#[test]
fn remote_baseline_equals_in_process_baseline() {
    let tree = full_grid(3, 2, 2);

    let mut local = Replay::new(&tree, 1000);
    BaselineParallelRefine.solve(&mut local).unwrap();
    let local_trace = local.finish();

    let (host_w, client_r) = pipe();
    let (client_w, mut host_r) = pipe();
    let client = std::thread::spawn(move || {
        let mut q = RemoteQuestion::connect(client_r, client_w);
        BaselineParallelRefine.solve(&mut q).unwrap();
        let observed = q.observed();
        q.send(&ClientMessage::Done);
        observed
    });
    let mut remote = Replay::new(&tree, 1000);
    let mut host_w = host_w;
    let outcome = serve_question(
        &mut remote,
        &PolicyConfig { beta: 0.5 },
        &mut host_r,
        &mut host_w,
        &ServeLimits::default(),
    )
    .unwrap();
    drop(host_w);
    let client_observed = client.join().unwrap();
    assert_eq!(outcome.ended_by, EndReason::Done);
    assert_eq!(outcome.illegal_batches, 0);
    let remote_observed = remote.observed();
    assert_eq!(remote.finish(), local_trace);
    assert_eq!(client_observed, remote_observed);
}

/// Serve a scripted client (input lines) against a fresh full-grid replay.
fn serve_script(
    input: &str,
    limits: ServeLimits,
    k2: u32,
) -> (Result<super::protocol::ServeOutcome, ServeError>, String) {
    let tree = full_grid(2, 1, 2);
    let mut replay = Replay::new(&tree, k2);
    let mut reader = Cursor::new(input.as_bytes().to_vec());
    let mut out = Vec::new();
    let res = serve_question(
        &mut replay,
        &PolicyConfig { beta: 0.0 },
        &mut reader,
        &mut out,
        &limits,
    );
    (res, String::from_utf8(out).unwrap())
}

fn violation(res: Result<super::protocol::ServeOutcome, ServeError>) -> ProtocolViolation {
    match res {
        Err(ServeError::Violation(v)) => v,
        other => panic!("expected violation, got {other:?}"),
    }
}

#[test]
fn scripted_happy_path_and_init_line() {
    let (res, out) = serve_script(
        "{\"op\":\"legal_roots\"}\n{\"op\":\"probe_batch\",\"cells\":[\"r1-b0-a0\"]}\n{\"op\":\"meta\",\"cell_id\":\"r1-b1-a1\"}\n{\"op\":\"done\"}\n",
        ServeLimits::default(),
        1000,
    );
    let outcome = res.unwrap();
    assert_eq!(outcome.ended_by, EndReason::Done);
    assert_eq!(outcome.requests, 4);
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 4, "init + three responses");
    let init: serde_json::Value = serde_json::from_str(lines[0]).unwrap();
    assert_eq!(init["op"], "init");
    assert_eq!(init["mode"], "solve");
    assert_eq!(init["max_parallelism"], 2);
    assert_eq!(init["config"]["beta"], 0.0);
    let meta: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
    assert_eq!(meta["ok"], false);
    assert_eq!(meta["error"], "meta_unavailable");
}

#[test]
fn invalid_json_is_a_violation() {
    let (res, _) = serve_script(
        "{\"op\":\"observed\"}\nnot json\n",
        ServeLimits::default(),
        1000,
    );
    assert_eq!(violation(res), ProtocolViolation::InvalidJson { line: 2 });
}

#[test]
fn unknown_op_is_a_violation() {
    let (res, _) = serve_script("{\"op\":\"peek_world\"}\n", ServeLimits::default(), 1000);
    assert!(
        matches!(violation(res), ProtocolViolation::UnknownOp { op, .. } if op == "peek_world")
    );
    let (res, _) = serve_script("{\"cells\":[]}\n", ServeLimits::default(), 1000);
    assert!(matches!(
        violation(res),
        ProtocolViolation::UnknownOp { .. }
    ));
}

#[test]
fn malformed_known_op_is_a_violation() {
    let (res, _) = serve_script(
        "{\"op\":\"probe_batch\",\"cells\":5}\n",
        ServeLimits::default(),
        1000,
    );
    assert!(
        matches!(violation(res), ProtocolViolation::MalformedRequest { op, .. } if op == "probe_batch")
    );
}

#[test]
fn plan_in_solve_mode_is_a_violation() {
    let (res, _) = serve_script(
        "{\"op\":\"plan\",\"branch_count\":1,\"refine_count\":1}\n",
        ServeLimits::default(),
        1000,
    );
    assert!(matches!(
        violation(res),
        ProtocolViolation::UnexpectedOp { .. }
    ));
}

#[test]
fn over_long_line_is_a_violation() {
    let long = format!(
        "{{\"op\":\"meta\",\"cell_id\":\"{}\"}}\n",
        "x".repeat(super::protocol::MAX_LINE_BYTES)
    );
    let (res, _) = serve_script(&long, ServeLimits::default(), 1000);
    assert!(matches!(
        violation(res),
        ProtocolViolation::LineTooLong { line: 1, .. }
    ));
    // Exactly at the limit is fine (then EOF → missing done).
    let limits = ServeLimits {
        max_line_bytes: 17,
        ..ServeLimits::default()
    };
    let (res, _) = serve_script("{\"op\":\"observed\"}\n", limits, 1000);
    assert_eq!(violation(res), ProtocolViolation::MissingDone);
}

#[test]
fn two_consecutive_illegal_batches_is_a_violation() {
    let (res, out) = serve_script(
        "{\"op\":\"probe_batch\",\"cells\":[]}\n{\"op\":\"probe_batch\",\"cells\":[\"r1-b0-a1\"]}\n",
        ServeLimits::default(),
        1000,
    );
    assert_eq!(
        violation(res),
        ProtocolViolation::ConsecutiveIllegalBatches { count: 2 }
    );
    assert_eq!(
        out.lines().filter(|l| l.contains("illegal_batch")).count(),
        2
    );
}

#[test]
fn illegal_legal_illegal_is_not_consecutive() {
    let (res, _) = serve_script(
        "{\"op\":\"probe_batch\",\"cells\":[]}\n{\"op\":\"probe_batch\",\"cells\":[\"r1-b0-a0\"]}\n{\"op\":\"probe_batch\",\"cells\":[]}\n{\"op\":\"done\"}\n",
        ServeLimits::default(),
        1000,
    );
    let outcome = res.unwrap();
    assert_eq!(outcome.illegal_batches, 2);
}

#[test]
fn eof_without_done_is_a_violation() {
    let (res, _) = serve_script("{\"op\":\"legal_actions\"}\n", ServeLimits::default(), 1000);
    assert_eq!(violation(res), ProtocolViolation::MissingDone);
    let (res, _) = serve_script("", ServeLimits::default(), 1000);
    assert_eq!(violation(res), ProtocolViolation::MissingDone);
}

#[test]
fn probe_after_k2_ends_the_session() {
    let (res, out) = serve_script(
        "{\"op\":\"probe_batch\",\"cells\":[\"r1-b0-a0\"]}\n{\"op\":\"probe_batch\",\"cells\":[\"r1-b1-a0\"]}\n",
        ServeLimits::default(),
        1,
    );
    assert_eq!(res.unwrap().ended_by, EndReason::K2Reached);
    assert!(out.lines().last().unwrap().contains("\"terminated\""));
}

#[test]
fn plan_grid_exchange() {
    let ctx = GridContext {
        history: Vec::new(),
        hard_max_branch_count: 4,
        hard_max_refine_count: 3,
        worker_cap: 4,
        trace_branch_count: None,
        trace_refine_count: None,
    };
    let mut out = Vec::new();
    let mut reader = Cursor::new(
        b"{\"op\":\"plan\",\"branch_count\":3,\"refine_count\":2,\"reason\":\"r\"}\n".to_vec(),
    );
    let plan = request_plan_grid(
        &PolicyConfig { beta: 0.6 },
        &ctx,
        &mut reader,
        &mut out,
        &ServeLimits::default(),
    )
    .unwrap();
    assert_eq!(
        (plan.branch_count, plan.refine_count, plan.reason.as_str()),
        (3, 2, "r")
    );
    let init: serde_json::Value =
        serde_json::from_slice(out.split(|b| *b == b'\n').next().unwrap()).unwrap();
    assert_eq!(init["mode"], "plan_grid");
    assert!(init["context"].get("trace_branch_count").is_none());

    let mut out = Vec::new();
    let mut reader = Cursor::new(Vec::new());
    let err = request_plan_grid(
        &PolicyConfig { beta: 0.6 },
        &ctx,
        &mut reader,
        &mut out,
        &ServeLimits::default(),
    )
    .unwrap_err();
    assert!(matches!(
        err,
        ServeError::Violation(ProtocolViolation::MissingPlan)
    ));
}
