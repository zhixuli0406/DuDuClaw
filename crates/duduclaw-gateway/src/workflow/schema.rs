//! Strict sequence definitions. Source material is data, never executable code.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_STEPS: usize = 64;
pub const MAX_JSON_BYTES: usize = 262_144;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum TypedSchema {
    Null,
    Boolean,
    Integer,
    Number,
    String {
        max_length: usize,
    },
    Array {
        items: Box<TypedSchema>,
        max_items: usize,
    },
    Object {
        properties: BTreeMap<String, TypedSchema>,
        required: BTreeSet<String>,
    },
    /// `null` or a value of `inner`. Receipt rows (a task row, a cron row)
    /// carry columns that are legitimately null; without this an output
    /// schema could never accept the real record. A nullable value is never
    /// a guaranteed reference source (`at_pointer` cannot step into it).
    Nullable {
        inner: Box<TypedSchema>,
    },
}
impl TypedSchema {
    pub fn validate_definition(&self, depth: usize) -> Result<(), String> {
        if depth > 32 {
            return Err("workflow schema depth exceeded".into());
        }
        match self {
            Self::String { max_length } if *max_length > MAX_JSON_BYTES => {
                return Err("workflow string bound exceeded".into());
            }
            Self::Array { items, max_items } => {
                if *max_items > 4096 {
                    return Err("workflow array bound exceeded".into());
                }
                items.validate_definition(depth + 1)?;
            }
            Self::Object {
                properties,
                required,
            } => {
                if properties.len() > 256 || !required.iter().all(|k| properties.contains_key(k)) {
                    return Err("invalid workflow object schema".into());
                }
                for child in properties.values() {
                    child.validate_definition(depth + 1)?;
                }
            }
            Self::Nullable { inner } => {
                if matches!(**inner, Self::Null | Self::Nullable { .. }) {
                    return Err("workflow nullable must wrap a non-null type".into());
                }
                inner.validate_definition(depth + 1)?;
            }
            _ => (),
        }
        Ok(())
    }
    fn at_pointer(&self, pointer: &str) -> Result<&Self, String> {
        if pointer.is_empty() {
            return Ok(self);
        }
        if !pointer.starts_with('/') {
            return Err("invalid workflow schema pointer".into());
        }
        let mut current = self;
        for raw in pointer[1..].split('/') {
            let mut key = String::new();
            let mut chars = raw.chars();
            while let Some(c) = chars.next() {
                if c == '~' {
                    key.push(match chars.next() {
                        Some('0') => '~',
                        Some('1') => '/',
                        _ => return Err("invalid workflow pointer escape".into()),
                    });
                } else {
                    key.push(c);
                }
            }
            current = match current {
                Self::Object {
                    properties,
                    required,
                } if required.contains(&key) => properties
                    .get(&key)
                    .ok_or("workflow reference field missing")?,
                // Array length has no guaranteed minimum; indexed values must be validated at runtime.
                _ => return Err("workflow reference is not guaranteed by source schema".into()),
            };
        }
        Ok(current)
    }
    fn fits(&self, destination: &Self) -> bool {
        match (self, destination) {
            (Self::Nullable { inner: a }, Self::Nullable { inner: b }) => a.fits(b),
            (Self::Null, Self::Nullable { .. }) => true,
            (_, Self::Nullable { inner: b }) => self.fits(b),
            (Self::Integer, Self::Number) => true,
            (Self::String { max_length: a }, Self::String { max_length: b }) => a <= b,
            (
                Self::Array {
                    items: a,
                    max_items: na,
                },
                Self::Array {
                    items: b,
                    max_items: nb,
                },
            ) => na <= nb && a.fits(b),
            (
                Self::Object {
                    properties: a,
                    required: ra,
                },
                Self::Object {
                    properties: b,
                    required: rb,
                },
            ) => {
                rb.is_subset(ra)
                    && a.iter()
                        .all(|(key, value)| b.get(key).is_some_and(|other| value.fits(other)))
            }
            _ => self == destination,
        }
    }
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        let valid = match self {
            Self::Null => value.is_null(),
            Self::Nullable { inner } => value.is_null() || inner.validate(value).is_ok(),
            Self::Boolean => value.is_boolean(),
            Self::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            Self::Number => value.is_number(),
            Self::String { max_length } => value
                .as_str()
                .is_some_and(|s| s.chars().count() <= *max_length),
            Self::Array { items, max_items } => value.as_array().is_some_and(|a| {
                a.len() <= *max_items && a.iter().all(|v| items.validate(v).is_ok())
            }),
            Self::Object {
                properties,
                required,
            } => value.as_object().is_some_and(|o| {
                required.iter().all(|k| o.contains_key(k))
                    && o.iter()
                        .all(|(k, v)| properties.get(k).is_some_and(|s| s.validate(v).is_ok()))
            }),
        };
        if valid {
            Ok(())
        } else {
            Err("workflow value violates typed schema".into())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case", deny_unknown_fields)]
pub enum InputRef {
    Literal { value: Value },
    Array { items: Vec<InputRef> },
    RunInput { pointer: String },
    StepOutput { step_id: String, pointer: String },
}

fn validate_input_reference(
    reference: &InputRef,
    destination: &TypedSchema,
    run_input: &TypedSchema,
    outputs: &BTreeMap<String, TypedSchema>,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    *nodes += 1;
    if depth > 32 || *nodes > 256 {
        return Err("workflow reference bounds exceeded".into());
    }
    match reference {
        InputRef::Literal { value } => destination.validate(value),
        InputRef::RunInput { pointer } => {
            if run_input.at_pointer(pointer)?.fits(destination) {
                Ok(())
            } else {
                Err("workflow input reference type mismatch".into())
            }
        }
        InputRef::StepOutput { step_id, pointer } => {
            let source = outputs
                .get(step_id)
                .ok_or("invalid forward/cyclic workflow reference")?
                .at_pointer(pointer)?;
            if source.fits(destination) {
                Ok(())
            } else {
                Err("workflow step reference type mismatch".into())
            }
        }
        InputRef::Array { items } => {
            let TypedSchema::Array {
                items: item_schema,
                max_items,
            } = destination
            else {
                return Err("workflow array reference requires array schema".into());
            };
            if items.len() > 64 || items.len() > *max_items {
                return Err("workflow array reference items exceeded".into());
            }
            for item in items {
                validate_input_reference(item, item_schema, run_input, outputs, depth + 1, nodes)?;
            }
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StepAction {
    McpRead { tool: String },
    McpEffect { tool: String, template_id: String },
    Process { transform: ProcessTransform },
    Artifact { label: String },
    Approval { summary: String },
    Question { summary: String },
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessTransform {
    Identity,
    Collect,
    Summary,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepDefinition {
    pub step_id: String,
    pub action: StepAction,
    pub input: InputRef,
    pub input_schema: TypedSchema,
    pub output_schema: TypedSchema,
    pub timeout_seconds: u32,
    pub max_read_attempts: u32,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowDefinition {
    pub schema_version: u32,
    pub workflow_id: String,
    pub revision: i64,
    pub skill_revision_hash: String,
    pub input_schema: TypedSchema,
    pub output_schema: TypedSchema,
    pub required_capabilities: BTreeSet<String>,
    pub steps: Vec<StepDefinition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreatorGrantSnapshot {
    pub actor: String,
    pub allowed_tools: BTreeSet<String>,
    pub policy_revision: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaskAuthorityRef {
    pub task_id: String,
    pub revision: i64,
    pub snapshot_hash: String,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedWorkflowRevision {
    pub definition: WorkflowDefinition,
    pub revision_hash: String,
    pub owner: String,
    pub creator_grant: CreatorGrantSnapshot,
    pub audience: Vec<String>,
    pub fixtures_digest: String,
    pub acceptance_id: String,
    pub accepted_at: String,
    pub expires_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FixtureKind {
    Normal,
    Empty,
    Expired,
    Injection,
    MissingPermission,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureExecutionRequest {
    pub fixture_id: String,
    pub draft_id: String,
    pub revision: i64,
    pub kind: FixtureKind,
    pub definition: WorkflowDefinition,
    pub input: Value,
    pub input_hash: String,
    pub assertion_hash: String,
    pub actor: String,
    pub creator_grant: CreatorGrantSnapshot,
    pub audience: Vec<String>,
    pub task: Option<TaskAuthorityRef>,
    pub deadline_at: String,
    pub budget: CostBudget,
    pub isolated_home: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostBudget {
    pub per_run_micros: u64,
    pub monthly_micros: u64,
    pub max_consecutive_failures: u32,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CostBreakdown {
    pub llm: u64,
    pub tools: u64,
    pub compute: u64,
    pub disk: u64,
    pub backup: u64,
    pub screenshot: u64,
    pub traffic: u64,
}
impl CostBreakdown {
    pub fn total(&self) -> Result<u64, String> {
        [
            self.llm,
            self.tools,
            self.compute,
            self.disk,
            self.backup,
            self.screenshot,
            self.traffic,
        ]
        .into_iter()
        .try_fold(0u64, |sum, cost| {
            sum.checked_add(cost).ok_or("workflow cost overflow".into())
        })
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Pending,
    Running,
    WaitingApproval,
    NeedsInput,
    Succeeded,
    Failed,
    Uncertain,
    Cancelled,
    Blocked,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    Running,
    WaitingApproval,
    NeedsInput,
    Succeeded,
    Failed,
    Uncertain,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionEvidenceKind {
    McpRead,
    Process,
    ArtifactCommit,
    OperationReceipt,
    HumanDecision,
    QuestionAnswer,
    GateDenial,
    None,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StepEvidence {
    pub step_id: String,
    pub status: StepStatus,
    pub input_hash: String,
    pub output_hash: Option<String>,
    pub output: Option<Value>,
    pub evidence_kind: ExecutionEvidenceKind,
    pub receipt: Option<Value>,
    pub operation_id: Option<String>,
    pub approval_id: Option<String>,
    pub cost: CostBreakdown,
    pub error_code: Option<String>,
    pub observed_at: String,
    /// Set when the step's operation outcome came from an administrator's
    /// uncertain-resolution rather than from the handler receipt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator_resolution: Option<Value>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureRunEvidence {
    pub fixture_id: String,
    pub draft_id: String,
    pub revision: i64,
    pub run_id: String,
    pub workflow_hash: String,
    pub skill_hash: String,
    pub input_hash: String,
    pub assertion_hash: String,
    pub policy_revision: String,
    pub kind: FixtureKind,
    pub status: RunStatus,
    pub steps: Vec<StepEvidence>,
    pub result_hash: Option<String>,
    pub expires_at: String,
    pub effect_call_count: u64,
    pub isolation: FixtureIsolationEvidence,
    pub execution_creator_grant: CreatorGrantSnapshot,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureIsolationEvidence {
    pub home_hash: String,
    pub environment_hash: String,
    pub isolated_staging: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AssertionOutcome {
    Matched,
    Mismatched,
    NotEvaluated,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureAssertion {
    pub assertion_id: String,
    pub expected_status: RunStatus,
    pub expected_error_code: Option<String>,
    pub expected_output_hash: Option<String>,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureAssertionResult {
    pub assertion: FixtureAssertion,
    pub outcome: AssertionOutcome,
    pub observed_status: RunStatus,
    pub observed_error_code: Option<String>,
    pub observed_output_hash: Option<String>,
    pub run_id: String,
}

impl WorkflowDefinition {
    pub fn parse(
        raw: &str,
        allowed_reads: &BTreeSet<String>,
        allowed_effects: &BTreeSet<String>,
    ) -> Result<Self, String> {
        let definition: Self =
            duduclaw_core::llm_contract::strict_json::parse_strict_with_limit(raw, MAX_JSON_BYTES)
                .map_err(|e| format!("invalid workflow JSON: {e:?}"))?;
        definition.validate(allowed_reads, allowed_effects)?;
        Ok(definition)
    }
    pub fn validate(
        &self,
        allowed_reads: &BTreeSet<String>,
        allowed_effects: &BTreeSet<String>,
    ) -> Result<(), String> {
        if self.schema_version != 1
            || self.revision < 1
            || self.workflow_id.is_empty()
            || self.skill_revision_hash.is_empty()
            || self.steps.is_empty()
            || self.steps.len() > MAX_STEPS
        {
            return Err("invalid workflow definition bounds".into());
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > MAX_JSON_BYTES {
            return Err("workflow definition byte bound exceeded".into());
        }
        self.input_schema.validate_definition(0)?;
        self.output_schema.validate_definition(0)?;
        let mut previous = BTreeSet::new();
        let mut output_schemas: BTreeMap<String, TypedSchema> = BTreeMap::new();
        let mut reference_nodes = 0usize;
        for step in &self.steps {
            if step.step_id.is_empty()
                // Artifact receipts and file names reuse the step id under the
                // agent-id rule (<=64), so the definition must not accept more.
                || step.step_id.len() > 64
                || !step
                    .step_id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
                || !previous.insert(step.step_id.clone())
                || !(1..=240).contains(&step.timeout_seconds)
                || !(1..=3).contains(&step.max_read_attempts)
            {
                return Err("invalid workflow step bounds".into());
            }
            step.input_schema.validate_definition(0)?;
            step.output_schema.validate_definition(0)?;
            match &step.action {
                StepAction::McpRead { tool } if !allowed_reads.contains(tool) => {
                    return Err("unsupported workflow read tool".into());
                }
                StepAction::McpEffect { tool, template_id }
                    if !allowed_effects.contains(tool) || template_id.is_empty() =>
                {
                    return Err("unsupported workflow effect tool".into());
                }
                _ => (),
            }
            validate_input_reference(
                &step.input,
                &step.input_schema,
                &self.input_schema,
                &output_schemas,
                0,
                &mut reference_nodes,
            )?;
            // Identity, artifact and approval steps pass their input through as
            // their output; a mismatch would only surface at run time.
            if matches!(
                &step.action,
                StepAction::Process {
                    transform: ProcessTransform::Identity
                } | StepAction::Artifact { .. }
                    | StepAction::Approval { .. }
            ) && !step.input_schema.fits(&step.output_schema)
            {
                return Err("workflow identity output type mismatch".into());
            }
            output_schemas.insert(step.step_id.clone(), step.output_schema.clone());
        }
        if !self
            .steps
            .last()
            .unwrap()
            .output_schema
            .fits(&self.output_schema)
        {
            return Err("workflow final output type mismatch".into());
        }
        Ok(())
    }
    pub fn hash(&self) -> String {
        crate::approval::payload_hash(&serde_json::to_value(self).expect("workflow serializable"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Trigger {
    Fixture {
        fixture_id: String,
        request_id: String,
    },
    Manual {
        request_id: String,
    },
    Scheduled {
        cron_id: String,
        timezone: String,
        scheduled_at: String,
    },
    Event {
        event_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowRun {
    pub run_id: String,
    pub trigger_key: String,
    pub trigger: Trigger,
    pub workflow_id: String,
    pub revision: i64,
    pub workflow_hash: String,
    pub skill_hash: String,
    pub actor: String,
    pub creator_grant: CreatorGrantSnapshot,
    pub audience: Vec<String>,
    pub task: Option<TaskAuthorityRef>,
    pub input: Value,
    pub input_hash: String,
    pub input_observed_at: String,
    pub policy_revision: String,
    pub environment_hash: String,
    pub grant: Option<crate::approval::GrantRef>,
    pub activation_id: Option<String>,
    pub deadline_at: String,
    pub budget: CostBudget,
    pub status: RunStatus,
    pub cost: CostBreakdown,
    pub error_code: Option<String>,
    pub created_at: String,
    #[serde(default)]
    pub decision_context: Option<crate::approval::DecisionContext>,
    /// Why a terminal run stopped, for the consecutive-failure breaker:
    /// `gate`, `definite`, `uncertain` or `transient`. Only `transient`
    /// (retries exhausted on infrastructure errors) is excluded from the count.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_class: Option<String>,
    /// Dashboard principal that cancelled this run (`dashboard:<user>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_by: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkflowEnvironment {
    pub schema_version: u32,
    pub binary_version: String,
    pub binary_hash: String,
    pub home: String,
    pub cwd: String,
    pub receipt_adapter_version: u32,
}
impl WorkflowEnvironment {
    pub fn hash(&self) -> String {
        crate::approval::payload_hash(
            &serde_json::to_value(self).expect("environment serializable"),
        )
    }
}
impl Trigger {
    pub fn key(&self, workflow: &str, revision: i64) -> Result<String, String> {
        let normalized = match self {
            Self::Scheduled {
                cron_id,
                timezone,
                scheduled_at,
            } => {
                timezone
                    .parse::<chrono_tz::Tz>()
                    .map_err(|_| "invalid workflow timezone")?;
                let slot = chrono::DateTime::parse_from_rfc3339(scheduled_at)
                    .map_err(|_| "invalid scheduled slot")?
                    .with_timezone(&chrono::Utc);
                serde_json::json!({"kind":"scheduled","cron_id":cron_id,"scheduled_at":slot.to_rfc3339()})
            }
            _ => serde_json::to_value(self).map_err(|e| e.to_string())?,
        };
        Ok(crate::approval::payload_hash(
            &serde_json::json!({"workflow":workflow,"revision":revision,"trigger":normalized}),
        ))
    }
}

#[cfg(test)]
mod nullable_tests {
    use super::TypedSchema;
    use serde_json::json;
    use std::collections::{BTreeMap, BTreeSet};

    fn nullable_string() -> TypedSchema {
        TypedSchema::Nullable {
            inner: Box::new(TypedSchema::String { max_length: 8 }),
        }
    }

    #[test]
    fn nullable_accepts_null_and_inner_only() {
        let schema = nullable_string();
        schema.validate_definition(0).unwrap();
        assert!(schema.validate(&json!(null)).is_ok());
        assert!(schema.validate(&json!("short")).is_ok());
        assert!(schema.validate(&json!("far too long")).is_err());
        assert!(schema.validate(&json!(1)).is_err());
        let doubled = TypedSchema::Nullable {
            inner: Box::new(nullable_string()),
        };
        assert!(doubled.validate_definition(0).is_err());
        let of_null = TypedSchema::Nullable {
            inner: Box::new(TypedSchema::Null),
        };
        assert!(of_null.validate_definition(0).is_err());
    }

    #[test]
    fn nullable_fits_only_into_nullable_or_wider() {
        let wide = TypedSchema::Nullable {
            inner: Box::new(TypedSchema::String { max_length: 64 }),
        };
        let exact = TypedSchema::String { max_length: 8 };
        assert!(nullable_string().fits(&wide));
        assert!(exact.fits(&wide));
        assert!(TypedSchema::Null.fits(&wide));
        // A value that may be null never satisfies a field that requires a string.
        assert!(!nullable_string().fits(&TypedSchema::String { max_length: 64 }));
    }

    #[test]
    fn nullable_field_is_not_a_guaranteed_reference_source() {
        let row = TypedSchema::Object {
            properties: BTreeMap::from([(
                "detail".to_string(),
                TypedSchema::Nullable {
                    inner: Box::new(TypedSchema::Object {
                        properties: BTreeMap::from([(
                            "id".to_string(),
                            TypedSchema::String { max_length: 8 },
                        )]),
                        required: BTreeSet::from(["id".to_string()]),
                    }),
                },
            )]),
            required: BTreeSet::from(["detail".to_string()]),
        };
        assert!(row.at_pointer("/detail").is_ok());
        assert!(row.at_pointer("/detail/id").is_err());
        assert!(row.validate(&json!({"detail": null})).is_ok());
    }
}
