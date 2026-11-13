//! Content-only parser. It must run outside the gateway uid (§10.2).
use std::io::{Read, Write};
use duduclaw_isolation::{ids::{EmployeeId, RequestId}, migration::{ImportBudget, ImportRecord, ImportedContent}};
use serde::{Deserialize, Serialize};

mod sqlite;
pub mod catalog;
pub mod authority;
pub use catalog::{take_schema_refusal, SchemaRefusal, SchemaVersion};
#[cfg(target_os = "linux")]
pub mod linux;

pub const MAX_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 8192;

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind { MemoryText, WikiText, ConversationText, TaskArchive, AuditArchive, SessionsSqlite, MemorySqlite, Attachment, WorkProduct, AuthorityJson, AuthorityToml, AuthorityUsersSqlite, AuthorityTasksSqlite, AuthorityApprovalsSqlite, AuthoritySessionsSqlite, AuthorityCronSqlite, AuthorityCronJsonl, AuthorityAutopilotSqlite, AuthorityWorkflowSqlite, AuthorityRemindersJsonl, AuthorityNotice, AuthorityInstructions, AuthoritySkill, AuthorityMcpKeysSqlite }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportRequest {
    pub version: u16,
    pub migration_id: RequestId,
    pub employee: Option<EmployeeId>,
    pub source_id: String,
    pub kind: SourceKind,
    pub title: String,
    pub legacy_agent: Option<String>,
    pub has_wal: bool,
    #[serde(default)]
    pub blob_handle: Option<RequestId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    #[error("invalid import request")] InvalidRequest,
    #[error("import limit exceeded")] Limit,
    #[error("import source unavailable")] Io,
    #[error("unsupported legacy schema")] Schema,
    #[error("invalid legacy content")] Content,
    #[error("import worker isolation unavailable")] Isolation,
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<std::io::Error> for Error { fn from(_: std::io::Error) -> Self { Self::Io } }
impl From<rusqlite::Error> for Error {
    fn from(_error: rusqlite::Error) -> Self {
        // Test builds only: name the SQLite cause that `Content` otherwise hides.
        #[cfg(test)] eprintln!("legacy sqlite error: {_error}");
        Self::Content
    }
}

impl ImportRequest {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_REQUEST_BYTES { return Err(Error::Limit); }
        let value: Self = serde_json::from_slice(bytes).map_err(|_| Error::InvalidRequest)?;
        value.validate()?;
        Ok(value)
    }
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 || self.source_id.is_empty() || self.source_id.len() > 400
            || self.title.len() > 1024 || self.legacy_agent.as_ref().is_some_and(|a| a.is_empty() || a.len() > 128)
            || (self.has_wal && !matches!(self.kind, SourceKind::SessionsSqlite | SourceKind::MemorySqlite | SourceKind::AuthorityUsersSqlite | SourceKind::AuthorityTasksSqlite | SourceKind::AuthorityApprovalsSqlite | SourceKind::AuthoritySessionsSqlite | SourceKind::AuthorityCronSqlite | SourceKind::AuthorityAutopilotSqlite | SourceKind::AuthorityWorkflowSqlite | SourceKind::AuthorityNotice | SourceKind::AuthorityMcpKeysSqlite))
            || (matches!(self.kind, SourceKind::SessionsSqlite | SourceKind::MemorySqlite) && (self.employee.is_none() || self.legacy_agent.is_none())) {
            return Err(Error::InvalidRequest);
        }
        if matches!(self.kind,SourceKind::Attachment|SourceKind::WorkProduct) != self.blob_handle.is_some() { return Err(Error::InvalidRequest); }
        Ok(())
    }
}

struct Emitter<'a, W> { request: &'a ImportRequest, output: W, budget: ImportBudget, sequence: u64 }
impl<W: Write> Emitter<'_, W> {
    fn emit(&mut self, source_suffix: &str, content: ImportedContent) -> Result<()> {
        let record = ImportRecord { schema_version: 1, migration_id: self.request.migration_id,
            sequence: self.sequence, employee: self.request.employee,
            source_record_id: format!("{}:{source_suffix}", self.request.source_id), content };
        record.validate().map_err(|_| Error::Limit)?;
        let bytes = serde_json::to_vec(&record).map_err(|_| Error::Content)?;
        self.budget.observe(bytes.len()).map_err(|_| Error::Limit)?;
        self.output.write_all(&bytes)?;
        self.output.write_all(b"\n")?;
        self.sequence += 1;
        Ok(())
    }
}

/// Output must be staged by the caller until this function and worker exit succeed.
pub fn import<R: Read, W: Write>(request: &ImportRequest, input: R, wal: Option<&mut dyn Read>, output: W) -> Result<u64> {
    catalog::clear_schema_refusal();
    request.validate()?;
    if request.has_wal != wal.is_some() { return Err(Error::InvalidRequest); }
    if matches!(request.kind,SourceKind::AuthorityJson|SourceKind::AuthorityToml|SourceKind::AuthorityUsersSqlite|SourceKind::AuthorityTasksSqlite|SourceKind::AuthorityApprovalsSqlite|SourceKind::AuthoritySessionsSqlite|SourceKind::AuthorityCronSqlite|SourceKind::AuthorityCronJsonl|SourceKind::AuthorityAutopilotSqlite|SourceKind::AuthorityWorkflowSqlite|SourceKind::AuthorityRemindersJsonl|SourceKind::AuthorityNotice|SourceKind::AuthorityInstructions|SourceKind::AuthoritySkill|SourceKind::AuthorityMcpKeysSqlite) {return authority::emit(request,input,wal,output);}
    let mut emitter = Emitter { request, output, budget: ImportBudget::default(), sequence: 0 };
    if matches!(request.kind, SourceKind::SessionsSqlite) {
        sqlite::sessions(input, wal, &mut emitter)?;
    } else if matches!(request.kind, SourceKind::MemorySqlite) {
        sqlite::memories(input, wal, &mut emitter)?;
    } else if matches!(request.kind,SourceKind::Attachment|SourceKind::WorkProduct) {
        use sha2::{Digest,Sha256};
        let mut input=input.take(MAX_SOURCE_BYTES+1);let mut buffer=[0u8;65536];let mut hash=Sha256::new();let mut size=0u64;
        loop {let n=input.read(&mut buffer)?;if n==0 {break;}size+=n as u64;if size>MAX_SOURCE_BYTES {return Err(Error::Limit);}hash.update(&buffer[..n]);}
        let blob=duduclaw_isolation::migration::ImportedBlob {source_handle:request.blob_handle.ok_or(Error::InvalidRequest)?,
            display_name:request.title.clone(),size_bytes:size,sha256:duduclaw_isolation::ids::Sha256Digest(hash.finalize().into())};
        let content=if matches!(request.kind,SourceKind::Attachment) {ImportedContent::Attachment {blob}} else {ImportedContent::WorkProduct {blob}};
        emitter.emit("blob",content)?;
    } else {
        let mut bytes = Vec::new();
        input.take(duduclaw_isolation::migration::MAX_IMPORT_RECORD_BYTES as u64 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > duduclaw_isolation::migration::MAX_IMPORT_RECORD_BYTES { return Err(Error::Limit); }
        let text = String::from_utf8(bytes).map_err(|_| Error::Content)?;
        let content = match request.kind {
            SourceKind::MemoryText => ImportedContent::Memory { text },
            SourceKind::WikiText => ImportedContent::WikiPage { title: request.title.clone(), text },
            SourceKind::ConversationText => ImportedContent::Conversation { text, recorded_at_unix: None },
            SourceKind::TaskArchive => ImportedContent::TaskHistory { text },
            SourceKind::AuditArchive => ImportedContent::LegacyAuditArchive { text },
            _ => return Err(Error::InvalidRequest),
        };
        emitter.emit("text", content)?;
    }
    emitter.output.flush()?;
    Ok(emitter.sequence)
}
