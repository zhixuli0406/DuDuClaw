//! Tool-result-aware context compression.
//!
//! Applies multi-fidelity classification to tool results before LLM-based compression,
//! dramatically reducing context consumption at zero LLM cost for the first pass.
//!
//! References:
//! - AFM (arXiv 2511.12712): three-tier fidelity (Full/Compressed/Placeholder)
//! - RECOMP (arXiv 2310.04408, ICLR 2024): selective augmentation, discard irrelevant
//! - ACON (arXiv 2510.00615): failure-driven guideline refinement
//! - Sculptor (arXiv 2508.04664): hide/restore mechanism

pub mod tool_classifier;
pub mod guidelines;

#[cfg(test)]
mod tests;
