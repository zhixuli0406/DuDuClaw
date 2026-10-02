//! Line-delimited JSON protocol between host and policy process (SPEC §7).
//!
//! The host is the server; the policy process is the client. This module
//! holds the wire types and a transport-agnostic host loop over any
//! `BufRead` / `Write` pair. Process spawning, sandboxing and wall-clock
//! limits belong to a later work package.

use std::io::{BufRead, Write};

use serde::{Deserialize, Serialize};

use super::policy::{GridContext, GridPlan, PolicyConfig, Question};
use super::replay::{CellMeta, IllegalBatchReason, Observation, ProbeError};

/// Hard cap on one protocol line (SPEC §7: 1 MiB).
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// `error` value of a rejected batch.
pub const ERR_ILLEGAL_BATCH: &str = "illegal_batch";
/// `error` value of a `meta` on a cell that is neither legal nor revealed.
pub const ERR_META_UNAVAILABLE: &str = "meta_unavailable";
/// `error` value of a `probe_batch` after the replay hit `K2`.
pub const ERR_TERMINATED: &str = "terminated";

/// Host → policy first line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum HostInit {
    Init(InitBody),
}

/// Body of the `init` line, discriminated by `mode`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum InitBody {
    Solve {
        config: PolicyConfig,
        max_parallelism: u32,
        baseline_score: f64,
    },
    PlanGrid {
        config: PolicyConfig,
        context: GridContext,
    },
}

/// Policy → host message.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ClientMessage {
    Observed,
    LegalActions,
    LegalRoots,
    OpenedBranches,
    Meta {
        cell_id: String,
    },
    ProbeBatch {
        cells: Vec<String>,
    },
    Done,
    Plan {
        branch_count: u32,
        refine_count: u32,
        #[serde(default)]
        reason: String,
    },
}

const CLIENT_OPS: [&str; 8] = [
    "observed",
    "legal_actions",
    "legal_roots",
    "opened_branches",
    "meta",
    "probe_batch",
    "done",
    "plan",
];

/// Host → policy response (one per request, `done` excepted).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HostResponse {
    Observed {
        ok: bool,
        observed: std::collections::BTreeMap<String, Observation>,
    },
    Cells {
        ok: bool,
        cells: Vec<String>,
    },
    Branches {
        ok: bool,
        branches: Vec<u32>,
    },
    Meta {
        ok: bool,
        meta: CellMeta,
    },
    Observations {
        ok: bool,
        observations: Vec<Observation>,
    },
    Error {
        ok: bool,
        error: String,
        detail: String,
    },
}

/// Host-side limits of the loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeLimits {
    pub max_line_bytes: usize,
    /// Consecutive `illegal_batch` results that invalidate the policy.
    pub max_consecutive_illegal: u32,
}

impl Default for ServeLimits {
    fn default() -> Self {
        Self {
            max_line_bytes: MAX_LINE_BYTES,
            max_consecutive_illegal: 2,
        }
    }
}

/// A host-side hard rule was broken: the policy version is invalid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProtocolViolation {
    #[error("line {line}: not valid JSON")]
    InvalidJson { line: u64 },
    #[error("line {line}: exceeds {limit} bytes")]
    LineTooLong { line: u64, limit: usize },
    #[error("line {line}: unknown op {op:?}")]
    UnknownOp { line: u64, op: String },
    #[error("line {line}: op {op:?} not allowed in this mode")]
    UnexpectedOp { line: u64, op: String },
    #[error("line {line}: malformed {op:?} request: {detail}")]
    MalformedRequest {
        line: u64,
        op: String,
        detail: String,
    },
    #[error("{count} consecutive illegal batches")]
    ConsecutiveIllegalBatches { count: u32 },
    #[error("policy stream ended without done")]
    MissingDone,
    #[error("policy stream ended without a plan line")]
    MissingPlan,
}

/// Failure of the host loop: a policy violation, or host-side I/O.
#[derive(Debug, thiserror::Error)]
pub enum ServeError {
    #[error("protocol violation: {0}")]
    Violation(#[from] ProtocolViolation),
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("encode: {0}")]
    Encode(#[from] serde_json::Error),
}

/// How a solve session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// The policy sent `done`.
    Done,
    /// A `probe_batch` arrived after `K2`; the host answered `terminated`
    /// and ended the replay.
    K2Reached,
}

/// Summary of a successful solve session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServeOutcome {
    pub ended_by: EndReason,
    pub requests: u64,
    pub illegal_batches: u64,
}

enum LineRead {
    Eof,
    Line(Vec<u8>),
    TooLong,
}

/// Read one `\n`-terminated line (a final unterminated line counts) without
/// ever buffering more than `max` content bytes. A trailing `\r` is dropped.
fn read_line_capped<R: BufRead>(reader: &mut R, max: usize) -> std::io::Result<LineRead> {
    let mut acc: Vec<u8> = Vec::new();
    let mut saw_bytes = false;
    loop {
        let buf = reader.fill_buf()?;
        if buf.is_empty() {
            return Ok(if saw_bytes {
                finish_line(acc)
            } else {
                LineRead::Eof
            });
        }
        saw_bytes = true;
        let (take, found) = match buf.iter().position(|&b| b == b'\n') {
            Some(pos) => (pos, true),
            None => (buf.len(), false),
        };
        if acc.len() + take > max {
            let consumed = take + usize::from(found);
            reader.consume(consumed);
            return Ok(LineRead::TooLong);
        }
        acc.extend_from_slice(&buf[..take]);
        reader.consume(take + usize::from(found));
        if found {
            return Ok(finish_line(acc));
        }
    }
}

fn finish_line(mut acc: Vec<u8>) -> LineRead {
    if acc.last() == Some(&b'\r') {
        acc.pop();
    }
    LineRead::Line(acc)
}

fn write_line<W: Write, T: Serialize>(writer: &mut W, value: &T) -> Result<(), ServeError> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

/// Read and decode the next client message. `Ok(None)` is end of stream.
fn next_message<R: BufRead>(
    reader: &mut R,
    limits: &ServeLimits,
    line_no: &mut u64,
) -> Result<Option<ClientMessage>, ServeError> {
    let raw = match read_line_capped(reader, limits.max_line_bytes)? {
        LineRead::Eof => return Ok(None),
        LineRead::TooLong => {
            *line_no += 1;
            return Err(ProtocolViolation::LineTooLong {
                line: *line_no,
                limit: limits.max_line_bytes,
            }
            .into());
        }
        LineRead::Line(bytes) => bytes,
    };
    *line_no += 1;
    let line = *line_no;
    let value: serde_json::Value =
        serde_json::from_slice(&raw).map_err(|_| ProtocolViolation::InvalidJson { line })?;
    let op = match value.get("op").and_then(serde_json::Value::as_str) {
        Some(op) => op.to_string(),
        None => {
            return Err(ProtocolViolation::UnknownOp {
                line,
                op: String::new(),
            }
            .into());
        }
    };
    if !CLIENT_OPS.contains(&op.as_str()) {
        return Err(ProtocolViolation::UnknownOp { line, op }.into());
    }
    serde_json::from_value::<ClientMessage>(value)
        .map(Some)
        .map_err(|e| {
            ProtocolViolation::MalformedRequest {
                line,
                op,
                detail: duduclaw_core::truncate_bytes(&e.to_string(), 200).to_string(),
            }
            .into()
        })
}

fn error_response(error: &str, detail: String) -> HostResponse {
    HostResponse::Error {
        ok: false,
        error: error.to_string(),
        detail,
    }
}

fn illegal_detail(reason: &IllegalBatchReason) -> String {
    reason.to_string()
}

/// Serve one `mode = "solve"` session: write `init`, then answer requests
/// against `question` until `done`, enforcing the host-side hard rules that
/// need no process (invalid JSON, unknown op, over-long line, two
/// consecutive illegal batches, missing `done`).
pub fn serve_question<R: BufRead, W: Write>(
    question: &mut dyn Question,
    config: &PolicyConfig,
    reader: &mut R,
    writer: &mut W,
    limits: &ServeLimits,
) -> Result<ServeOutcome, ServeError> {
    let init = HostInit::Init(InitBody::Solve {
        config: *config,
        max_parallelism: question.max_parallelism(),
        baseline_score: question.baseline_score(),
    });
    write_line(writer, &init)?;

    let mut line_no = 0_u64;
    let mut requests = 0_u64;
    let mut illegal_batches = 0_u64;
    let mut streak = 0_u32;
    loop {
        let Some(msg) = next_message(reader, limits, &mut line_no)? else {
            return Err(ProtocolViolation::MissingDone.into());
        };
        requests += 1;
        let response = match msg {
            ClientMessage::Done => {
                return Ok(ServeOutcome {
                    ended_by: EndReason::Done,
                    requests,
                    illegal_batches,
                });
            }
            ClientMessage::Plan { .. } => {
                return Err(ProtocolViolation::UnexpectedOp {
                    line: line_no,
                    op: "plan".to_string(),
                }
                .into());
            }
            ClientMessage::Observed => HostResponse::Observed {
                ok: true,
                observed: question.observed(),
            },
            ClientMessage::LegalActions => HostResponse::Cells {
                ok: true,
                cells: question.legal_actions(),
            },
            ClientMessage::LegalRoots => HostResponse::Cells {
                ok: true,
                cells: question.legal_roots(),
            },
            ClientMessage::OpenedBranches => HostResponse::Branches {
                ok: true,
                branches: question.opened_branches(),
            },
            ClientMessage::Meta { cell_id } => match question.meta(&cell_id) {
                Ok(meta) => HostResponse::Meta { ok: true, meta },
                Err(e) => error_response(ERR_META_UNAVAILABLE, e.to_string()),
            },
            ClientMessage::ProbeBatch { cells } => match question.probe_batch(&cells) {
                Ok(observations) => {
                    streak = 0;
                    HostResponse::Observations {
                        ok: true,
                        observations,
                    }
                }
                Err(ProbeError::IllegalBatch(reason)) => {
                    streak += 1;
                    illegal_batches += 1;
                    if streak >= limits.max_consecutive_illegal {
                        write_line(
                            writer,
                            &error_response(ERR_ILLEGAL_BATCH, illegal_detail(&reason)),
                        )?;
                        return Err(
                            ProtocolViolation::ConsecutiveIllegalBatches { count: streak }.into(),
                        );
                    }
                    error_response(ERR_ILLEGAL_BATCH, illegal_detail(&reason))
                }
                Err(ProbeError::Terminated(_)) => {
                    write_line(
                        writer,
                        &error_response(ERR_TERMINATED, "k2_reached".to_string()),
                    )?;
                    return Ok(ServeOutcome {
                        ended_by: EndReason::K2Reached,
                        requests,
                        illegal_batches,
                    });
                }
            },
        };
        write_line(writer, &response)?;
    }
}

/// Run one `mode = "plan_grid"` exchange: write `init`, read exactly one
/// `plan` line.
pub fn request_plan_grid<R: BufRead, W: Write>(
    config: &PolicyConfig,
    context: &GridContext,
    reader: &mut R,
    writer: &mut W,
    limits: &ServeLimits,
) -> Result<GridPlan, ServeError> {
    let init = HostInit::Init(InitBody::PlanGrid {
        config: *config,
        context: context.clone(),
    });
    write_line(writer, &init)?;
    let mut line_no = 0_u64;
    match next_message(reader, limits, &mut line_no)? {
        None => Err(ProtocolViolation::MissingPlan.into()),
        Some(ClientMessage::Plan {
            branch_count,
            refine_count,
            reason,
        }) => Ok(GridPlan {
            branch_count,
            refine_count,
            reason,
        }),
        Some(other) => Err(ProtocolViolation::UnexpectedOp {
            line: line_no,
            op: op_name(&other).to_string(),
        }
        .into()),
    }
}

fn op_name(msg: &ClientMessage) -> &'static str {
    match msg {
        ClientMessage::Observed => "observed",
        ClientMessage::LegalActions => "legal_actions",
        ClientMessage::LegalRoots => "legal_roots",
        ClientMessage::OpenedBranches => "opened_branches",
        ClientMessage::Meta { .. } => "meta",
        ClientMessage::ProbeBatch { .. } => "probe_batch",
        ClientMessage::Done => "done",
        ClientMessage::Plan { .. } => "plan",
    }
}
