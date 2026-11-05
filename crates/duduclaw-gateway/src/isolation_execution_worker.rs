use std::{fs::File, io::Read, sync::Arc};
use duduclaw_mcp_boundary::{VerificationError, factories::GatewayTableParser,
    parsing::{FileParser, TableKind, TableOptions}, tool_policy::ResourceResolver,
    worker_execution::{WorkerPlan, WorkerReply, WorkerInvoke, WorkerToolBridge, WorkerPlanFactory}};
use duduclaw_parse_worker::{client::{self, WorkerLauncher}, worker::{self, ParseRequest, ParseResponse,
    ParseOperation, ParseOptions, ParseResult, ParsedItem, TableCell}};
use serde_json::{Value, json};

/// Trusted boot only. E's OnceLock refuses replacement and its session remains
/// alive through snapshot, response validation and output verification. An
/// installation failure returns no adapter, so no socket-default fallback can
/// become reachable from D. The gateway never enables E's `engine` feature.
pub fn install_worker(launcher: Box<dyn WorkerLauncher>)
    -> Result<Arc<dyn FileParser>, VerificationError> {
    Ok(install_worker_services(launcher)?.parser)
}

pub struct InstalledWorkerServices {
    pub parser: Arc<dyn FileParser>,
    invoke: Arc<WorkerInvoke>,
}
impl InstalledWorkerServices {
    pub fn tools(&self, factory: Arc<dyn WorkerPlanFactory>, resolver: Arc<dyn ResourceResolver>)
        -> Arc<WorkerToolBridge> {
        Arc::new(WorkerToolBridge::new(factory, resolver, self.invoke.clone()))
    }
}

/// Install once and use both the fd-table path and all factory-registered E
/// operations. A missing per-tool factory entry remains absent from tools/list.
pub fn install_worker_services(launcher: Box<dyn WorkerLauncher>)
    -> Result<InstalledWorkerServices, VerificationError> {
    client::install_launcher(launcher).map_err(|_| VerificationError::Unavailable)?;
    client::require_isolation();
    Ok(InstalledWorkerServices {
        parser: Arc::new(GatewayTableParser::new(Arc::new(invoke_table))),
        invoke: Arc::new(invoke_operation),
    })
}

fn invoke_operation(plan: WorkerPlan) -> Result<WorkerReply, VerificationError> {
    duduclaw_parse_worker::policy::validate(&plan.request).map_err(|_| VerificationError::Invalid)?;
    if plan.request.inputs.as_slice().len() != 1 { return Err(VerificationError::Invalid); }
    let mut input = LimitedInput { file: plan.input,
        remaining: plan.request.inputs.as_slice()[0].size_bytes.try_into().map_err(|_| VerificationError::Invalid)? };
    let (response, outputs) = client::invoke_output_reader(&plan.request, &mut input)
        .map_err(|_| VerificationError::Unavailable)?;
    Ok(WorkerReply { response, outputs })
}

fn invoke_table(mut request: ParseRequest, file: File, options: TableOptions)
    -> Result<Value, VerificationError> {
    options.validate()?;
    request.operation = ParseOperation::ReadTable;
    request.options = Some(ParseOptions::Table(worker::TableOptions {
        format: match options.kind { TableKind::Csv => worker::TableFormat::Csv,
            TableKind::Spreadsheet => worker::TableFormat::Spreadsheet },
        delimiter: options.delimiter, has_header: options.has_header,
        offset: options.offset.try_into().map_err(|_| VerificationError::Invalid)?,
        limit: options.limit.try_into().map_err(|_| VerificationError::Invalid)?,
        sheet: options.sheet.clone().map(duduclaw_parse_worker::bounded::Text::new)
            .transpose().map_err(|_| VerificationError::Invalid)?,
    }));
    request.validate().map_err(|_| VerificationError::Invalid)?;
    // E reserves the full input ceiling before copying this authorized fd to a
    // sealed snapshot and exchanging it with its injected WorkerSession.
    let cap = match options.kind { TableKind::Csv => 64 * 1024 * 1024, TableKind::Spreadsheet => 32 * 1024 * 1024 };
    let mut input = LimitedInput { file, remaining: cap };
    let (response, outputs) = client::invoke_output_reader(&request, &mut input)
        .map_err(|_| VerificationError::Unavailable)?;
    if !outputs.is_empty() { return Err(VerificationError::Invalid); }
    table_value(&request, &options, response)
}

/// Stop growth between metadata authorization and E's snapshot. A limit hit is
/// an error, never EOF/truncation that could be mistaken for a complete table.
struct LimitedInput { file: File, remaining: usize }
impl Read for LimitedInput {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() { return Ok(0); }
        if self.remaining == 0 {
            let mut extra = [0; 1];
            return match self.file.read(&mut extra)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other("worker input limit")),
            };
        }
        let count = output.len().min(self.remaining);
        let bytes = self.file.read(&mut output[..count])?;
        self.remaining -= bytes;
        Ok(bytes)
    }
}

fn table_value(request: &ParseRequest, options: &TableOptions, response: ParseResponse)
    -> Result<Value, VerificationError> {
    response.validate_for(request).map_err(|_| VerificationError::Invalid)?;
    let ParseResult::Complete { items } = response.result else { return Err(VerificationError::Unavailable); };
    let [ParsedItem::Table { input_slot: 0, output }] = items.as_slice() else {
        return Err(VerificationError::Invalid);
    };
    let columns: Vec<&str> = output.columns.as_slice().iter().map(|s| s.as_str()).collect();
    let mut rows = Vec::new();
    for row in output.rows.as_slice() {
        if row.as_slice().len() != columns.len() { return Err(VerificationError::Invalid); }
        let mut object = serde_json::Map::new();
        for (name, cell) in columns.iter().zip(row.as_slice()) {
            let value = match cell {
                TableCell::Null => Value::Null,
                TableCell::Bool(value) => json!(value),
                TableCell::Number(value) => json!(value),
                TableCell::String(value) => json!(value.as_str()),
            };
            object.insert((*name).to_owned(), value);
        }
        rows.push(Value::Object(object));
    }
    let mut value = json!({"columns":columns,"rows":rows,"truncated":output.truncated});
    if options.kind == TableKind::Spreadsheet {
        value["sheet"] = json!(output.sheet.as_ref().map(|s| s.as_str()));
        value["sheets"] = json!(output.sheets.as_slice().iter().map(|s| s.as_str()).collect::<Vec<_>>());
    }
    Ok(value)
}

#[cfg(test)]
#[path = "isolation_execution_worker_tests.rs"]
mod tests;
