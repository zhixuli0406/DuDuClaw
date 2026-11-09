//! Bounded selectors are data for the disposable HTML parser, never host paths.
use serde::{Deserialize, Serialize};
use crate::bounded::Text;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HtmlOutputFormat { Text, Html, Json, Element }

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlQuery {
    pub name: Text<256>,
    pub selector: Text<2048>,
    pub format: HtmlOutputFormat,
    /// Zero requests all matches, subject to the response's hard item ceiling.
    pub max_matches: u32,
}
