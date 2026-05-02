//! Governance 整合測試模組
//!
//! M1-A：PolicyRegistry + YAML 規則載入 TDD 整合測試。
//! M1-B：PolicyEvaluator + ViolationDetector TDD 整合測試。
//! M1-C：ApprovalWorkflow + QuotaManager TDD 整合測試。

pub mod policy_registry_integration;
pub mod policy_evaluator_integration;
pub mod approval_workflow_integration;
