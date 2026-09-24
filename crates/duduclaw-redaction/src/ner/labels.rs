//! OpenAI Privacy Filter label scheme and its mapping onto DuDuClaw
//! redaction categories.
//!
//! The model emits BIOES tags over eight entity types. Everything downstream
//! (tokens, `[redaction.sources] only_categories`, the dashboard's category
//! chips, i18n keys `redaction.cat.*`) speaks DuDuClaw's `[A-Z0-9_]{1,32}`
//! category ids instead, so this is the one translation table.
//!
//! Deliberately a plain table rather than config: an operator renaming
//! `private_person` to something else would silently detach every existing
//! vault entry and source filter from the rule that made it.

/// One entity type the model can emit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NerLabel {
    /// The label as it appears in `config.json`'s `id2label`
    /// (`B-private_person` → entity `private_person`).
    pub model_label: &'static str,
    /// DuDuClaw category carried in the token (`<REDACT:PERSON:…>`).
    pub category: &'static str,
}

/// All eight labels, in the order the design doc (§5) lists them.
///
/// `EMAIL` and `URL` deliberately reuse the categories the built-in regex
/// rules already emit: a model-found email and a regex-found email are the
/// same kind of data and must tokenise into the same category, or a source
/// filter naming `EMAIL` would cover only half of them.
pub const NER_LABELS: &[NerLabel] = &[
    NerLabel { model_label: "private_person", category: "PERSON" },
    NerLabel { model_label: "private_address", category: "ADDRESS" },
    NerLabel { model_label: "private_email", category: "EMAIL" },
    NerLabel { model_label: "private_phone", category: "PHONE" },
    NerLabel { model_label: "private_url", category: "URL" },
    NerLabel { model_label: "private_date", category: "DATE" },
    NerLabel { model_label: "account_number", category: "ACCOUNT_NUMBER" },
    NerLabel { model_label: "secret", category: "SECRET" },
];

/// Model label → DuDuClaw category. `None` for an unknown label (a model
/// revision that grew a ninth type would surface here rather than leaking
/// spans with no category).
pub fn category_for(model_label: &str) -> Option<&'static str> {
    NER_LABELS
        .iter()
        .find(|l| l.model_label == model_label)
        .map(|l| l.category)
}

/// Resolve a rule's `labels = [...]` list against the table.
///
/// An empty list means "all eight" (the documented default). An unknown
/// entry is an error, not a skip: a typo'd `private_persons` that silently
/// matched nothing would look exactly like a working rule at runtime.
pub fn resolve_labels(requested: &[String]) -> Result<Vec<&'static NerLabel>, String> {
    if requested.is_empty() {
        return Ok(NER_LABELS.iter().collect());
    }
    let mut out: Vec<&'static NerLabel> = Vec::with_capacity(requested.len());
    for want in requested {
        let want = want.trim();
        let Some(found) = NER_LABELS.iter().find(|l| l.model_label == want) else {
            return Err(format!(
                "unknown NER label '{want}' (expected one of: {})",
                NER_LABELS
                    .iter()
                    .map(|l| l.model_label)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        };
        if !out.iter().any(|l| l.model_label == found.model_label) {
            out.push(found);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_label_maps_to_a_valid_category_id() {
        for l in NER_LABELS {
            assert!(
                crate::custom_rules::is_valid_category(l.category),
                "category '{}' is not a legal token category",
                l.category
            );
        }
    }

    #[test]
    fn design_doc_mapping_is_exact() {
        // §5 of the design doc, transcribed. A change here is a wire change:
        // existing vault entries carry the old category in their token.
        let expect = [
            ("private_person", "PERSON"),
            ("private_address", "ADDRESS"),
            ("private_email", "EMAIL"),
            ("private_phone", "PHONE"),
            ("private_url", "URL"),
            ("private_date", "DATE"),
            ("account_number", "ACCOUNT_NUMBER"),
            ("secret", "SECRET"),
        ];
        assert_eq!(NER_LABELS.len(), expect.len());
        for (label, cat) in expect {
            assert_eq!(category_for(label), Some(cat), "mapping drift for {label}");
        }
    }

    #[test]
    fn unknown_label_has_no_category() {
        assert_eq!(category_for("private_persons"), None);
        assert_eq!(category_for(""), None);
    }

    #[test]
    fn empty_request_resolves_to_all_eight() {
        assert_eq!(resolve_labels(&[]).unwrap().len(), 8);
    }

    #[test]
    fn subset_request_resolves_in_request_order_and_dedups() {
        let got = resolve_labels(&[
            "private_phone".into(),
            "private_person".into(),
            "private_phone".into(),
        ])
        .unwrap();
        assert_eq!(
            got.iter().map(|l| l.category).collect::<Vec<_>>(),
            vec!["PHONE", "PERSON"]
        );
    }

    #[test]
    fn unknown_label_is_an_error_not_a_skip() {
        let err = resolve_labels(&["private_persons".into()]).unwrap_err();
        assert!(err.contains("private_persons"), "{err}");
    }
}
