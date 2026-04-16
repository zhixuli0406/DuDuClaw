//! Shared sensitive pattern definitions for skill security checks.
//!
//! Used by `synthesizer.rs`, `vetting.rs`, and `security_scanner.rs` to ensure
//! consistent detection coverage across all code paths.

/// Severity level for a sensitive pattern match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternSeverity {
    /// Blocks installation/graduation.
    Critical,
    /// Logged as warning, does not block.
    Warning,
}

/// A sensitive pattern with its description and severity.
pub struct SensitivePattern {
    /// Pattern to match (lowercase for case-insensitive comparison).
    pub pattern: &'static str,
    /// Human-readable description.
    pub description: &'static str,
    /// Severity when matched.
    pub severity: PatternSeverity,
}

/// Secret/credential patterns. All patterns are lowercase for case-insensitive matching.
pub const SECRET_PATTERNS: &[SensitivePattern] = &[
    SensitivePattern { pattern: "sk-ant-", description: "Anthropic API key", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "sk-proj-", description: "OpenAI project key", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "api_key=", description: "API key assignment", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "password=", description: "Password assignment", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "anthropic_api_key", description: "Anthropic env var", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "openai_api_key", description: "OpenAI env var", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "ghp_", description: "GitHub personal access token", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "gho_", description: "GitHub OAuth token", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "github_pat_", description: "GitHub fine-grained PAT", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "xoxb-", description: "Slack bot token", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "xoxp-", description: "Slack user token", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "akia", description: "AWS access key ID", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "sk_test_", description: "Stripe test key", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "sk_live_", description: "Stripe live key", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "-----begin", description: "PEM private key", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "glpat-", description: "GitLab PAT", severity: PatternSeverity::Critical },
    SensitivePattern { pattern: "aiza", description: "Google API key", severity: PatternSeverity::Critical },
    // Short/ambiguous patterns: Warning only (may match normal text)
    SensitivePattern { pattern: "eyj", description: "JWT token (base64 header)", severity: PatternSeverity::Warning },
    SensitivePattern { pattern: "sg.", description: "SendGrid API key (verify manually)", severity: PatternSeverity::Warning },
];

/// Prompt injection patterns. All patterns are lowercase.
pub const PROMPT_INJECTION_PATTERNS: &[(&str, &str)] = &[
    ("ignore previous", "Instruction override"),
    ("ignore above", "Instruction override"),
    ("disregard all", "Instruction override"),
    ("forget your instructions", "Instruction override"),
    ("system:", "System role injection"),
    ("assistant:", "Assistant role injection"),
    ("<|im_start|>", "ChatML injection"),
    ("<|im_end|>", "ChatML injection"),
    ("[inst]", "Llama format injection"),
    ("<<sys>>", "Llama system injection"),
    ("</s>", "EOS token injection"),
    ("you are now", "Role hijacking"),
    ("act as if", "Role hijacking"),
    ("pretend to be", "Role hijacking"),
    ("new instructions:", "Instruction injection"),
    ("override all", "Instruction override"),
    ("jailbreak", "Jailbreak attempt"),
];

/// Code execution patterns. All patterns are lowercase.
pub const CODE_EXECUTION_PATTERNS: &[(&str, &str)] = &[
    ("os.system(", "Python os.system"),
    ("subprocess.run(", "Python subprocess"),
    ("subprocess.popen(", "Python subprocess"),
    ("subprocess.call(", "Python subprocess"),
    ("eval(", "Dynamic code eval"),
    ("exec(", "Dynamic code exec"),
    ("import os", "Python os import"),
    ("import subprocess", "Python subprocess import"),
    ("import shutil", "Python shutil import"),
    ("child_process", "Node.js child_process"),
    ("require('fs')", "Node.js filesystem"),
    ("require(\"fs\")", "Node.js filesystem"),
    ("deno.run", "Deno subprocess"),
    ("runtime.getruntime().exec", "Java process exec"),
    ("processbuilder", "Java process builder"),
    ("__import__", "Python dynamic import"),
];
