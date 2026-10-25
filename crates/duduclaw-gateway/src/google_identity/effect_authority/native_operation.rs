//! One-shot native execution from the already-consumed host claim.
use super::*;
use crate::google_workspace::native_call::{NativeCallTemplate, tool_error};
use serde_json::{Value, json};

pub(crate) enum ClosedNativeFailure {
    Denied(Refusal),
    Unconfirmed,
}
impl ClosedNativeFailure {
    pub(crate) fn validation(error: Refusal, may_have_sent: bool) -> Self {
        if may_have_sent {
            Self::Unconfirmed
        } else {
            Self::Denied(error)
        }
    }
    pub(crate) fn refused(may_have_sent: bool) -> Self {
        Self::validation(
            Refusal::hard(
                "native_request_refused",
                "Google 操作無法安全完成，請重新確認帳號與參數。",
            ),
            may_have_sent,
        )
    }
    fn into_refusal(self) -> Refusal {
        match self {
            Self::Denied(error) => error,
            Self::Unconfirmed => Refusal::hard(
                "result_unconfirmed",
                "Google 操作結果無法確認。若這是寫入，請先到 Google 檢查是否已完成，勿直接重試。",
            ),
        }
    }
}

impl PendingGoogleCall {
    pub(in crate::google_identity) async fn execute_native(
        self,
        arguments: Value,
    ) -> Result<Value, Refusal> {
        self.context.validate()?;
        self.claim.validate(&self.context, Clock::now())?;
        let GooglePurpose::Dedicated {
            native_tool,
            final_arguments_digest,
            ..
        } = &self.context.purpose
        else {
            return Err(changed());
        };
        if gi::google_arguments_digest(&arguments) != *final_arguments_digest {
            return Err(changed());
        }
        let template = match NativeCallTemplate::prepare(native_tool, &arguments).and_then(|t| {
            t.validate_closed()?;
            Ok(t)
        }) {
            Ok(template) => template,
            Err(message) => {
                // Bad arguments never open material or acquire an effect grant.
                self.context.validate()?;
                self.claim.validate(&self.context, Clock::now())?;
                crate::google_identity::execute::audit(
                    &self.context.home,
                    &self.context.agent,
                    native_tool,
                    "",
                    false,
                    false,
                    "native_arguments_invalid",
                );
                return Ok(tool_error(&message));
            }
        };
        if !crate::google_workspace::native_legacy::first_recipe_fits(&template).await {
            self.context.validate()?;
            self.claim.validate(&self.context, Clock::now())?;
            crate::google_identity::execute::audit(
                &self.context.home,
                &self.context.agent,
                native_tool,
                "",
                false,
                false,
                "native_recipe_too_large",
            );
            return Ok(tool_error("Google 請求的網址與本文超過 1 MiB 上限。"));
        }
        #[cfg(test)]
        if !crate::google_workspace::native_call::testing::required(&self.context.home) {
            // The old closed-refusal regression remains a missing-owned-fixture
            // regression; a unit test can never fall back to a real provider.
            return Err(Refusal::hard(
                "personal_external_unsupported",
                "此 Google 專用連線的封閉送出介面仍在驗收中，這次沒有送出 Google 操作。",
            ));
        }
        let authority = self.resolve().await?.acquire_effect().await?;
        authority
            .execute_native(template)
            .await
            .map_err(ClosedNativeFailure::into_refusal)
    }
}

impl GoogleEffectAuthority {
    pub(crate) fn validate_native_template(
        &self,
        template: &NativeCallTemplate,
    ) -> Result<(), Refusal> {
        let GooglePurpose::Dedicated {
            native_tool,
            final_arguments_digest,
            credential_ref,
        } = &self.material.context.purpose
        else {
            return Err(changed());
        };
        if template.tool() != native_tool || template.digest() != final_arguments_digest {
            return Err(changed());
        }
        self.validate_dedicated(native_tool, final_arguments_digest, credential_ref)
    }
    async fn execute_native(
        self,
        template: NativeCallTemplate,
    ) -> Result<Value, ClosedNativeFailure> {
        self.validate_native_template(&template)
            .map_err(|e| ClosedNativeFailure::validation(e, false))?;
        let fallback = match &self.material.claim {
            MaterialClaim::Dedicated(claim) => claim.is_home_fallback(),
            _ => false,
        };
        let result = match &self.material.selected {
            Material::Variant(material) => material.native(&self, &template).await,
            Material::HomeOAuth(_) => {
                let GooglePurpose::Dedicated { credential_ref, .. } =
                    &self.material.context.purpose
                else {
                    return Err(ClosedNativeFailure::refused(false));
                };
                let view = self
                    .borrow_home_native(template.tool(), template.digest(), credential_ref)
                    .map_err(|e| ClosedNativeFailure::validation(e, false))?;
                view.execute_native(&template)
                    .await
                    .map(|done| done.into_value())
            }
            Material::Personal(token) => {
                let permit = PersonalNativeEffectPermit { authority: &self };
                token
                    .execute_native(permit, &template)
                    .await
                    .map(|done| done.into_value())
            }
        };
        let value = result?;
        #[cfg(test)]
        crate::google_workspace::native_call::testing::boundary(
            &self.material.context.home,
            "e1_after_owner",
        );
        // A completed owner result implies the HTTP program may have run.
        if serde_json::to_vec(&json!({"ok":true,"result":&value}))
            .map_or(true, |b| b.len() > 512 * 1024)
        {
            return Err(ClosedNativeFailure::Unconfirmed);
        }
        self.validate_native_template(&template)
            .map_err(|_| ClosedNativeFailure::Unconfirmed)?;
        crate::google_identity::execute::audit(
            &self.material.context.home,
            &self.material.context.agent,
            template.tool(),
            match &self.material.claim {
                MaterialClaim::Dedicated(claim) => claim.audit_account(),
                _ => "",
            },
            value.get("isError").and_then(Value::as_bool) != Some(true),
            fallback,
            "",
        );
        Ok(value)
    }
}

pub(crate) struct PersonalNativeEffectPermit<'a> {
    authority: &'a GoogleEffectAuthority,
}
impl PersonalNativeEffectPermit<'_> {
    pub(crate) fn validate(
        &self,
        actual: &PinnedPersonalToken,
        template: &NativeCallTemplate,
    ) -> Result<(), Refusal> {
        match &self.authority.material.selected {
            Material::Personal(expected) if std::ptr::eq(expected, actual) => {
                self.authority.validate_native_template(template)
            }
            _ => Err(changed()),
        }
    }
}
impl HomeOAuthEffectPermit<'_> {
    pub(crate) fn validate_native_template(
        &self,
        actual: &HomeOAuthResolved,
        template: &NativeCallTemplate,
    ) -> Result<(), Refusal> {
        self.validate_home_oauth(actual)?;
        self.authority.validate_native_template(template)
    }
}

#[cfg(test)]
impl PersonalNativeEffectPermit<'_> {
    pub(crate) fn test_endpoint(&self, original: &str) -> Option<reqwest::Url> {
        crate::google_workspace::native_call::testing::endpoint(
            &self.authority.material.context.home,
            original,
        )
    }
    pub(crate) fn test_boundary(&self, stage: &str) {
        crate::google_workspace::native_call::testing::boundary(
            &self.authority.material.context.home,
            stage,
        )
    }
    pub(crate) fn test_completed(&self, value: &Value) {
        crate::google_workspace::native_call::testing::completed(
            &self.authority.material.context.home,
            value,
        )
    }
}

#[cfg(test)]
impl HomeOAuthEffectPermit<'_> {
    pub(crate) fn test_endpoint(&self, original: &str) -> Option<reqwest::Url> {
        crate::google_workspace::native_call::testing::endpoint(
            &self.authority.material.context.home,
            original,
        )
    }
    pub(crate) fn test_boundary(&self, stage: &str) {
        crate::google_workspace::native_call::testing::boundary(
            &self.authority.material.context.home,
            stage,
        )
    }
    pub(crate) fn test_completed(&self, value: &Value) {
        crate::google_workspace::native_call::testing::completed(
            &self.authority.material.context.home,
            value,
        )
    }
}

#[cfg(test)]
impl GoogleEffectAuthority {
    // Exercise one foreign step without exposing a reusable permit factory.
    pub(super) async fn test_foreign_native_step(
        &self,
        expected: &NativeCallTemplate,
        other: &NativeCallTemplate,
    ) -> bool {
        match &self.material.selected {
            Material::Variant(material) => material.test_foreign(self, expected, other).await,
            Material::Personal(token) => {
                token
                    .test_foreign_native_step(
                        PersonalNativeEffectPermit { authority: self },
                        expected,
                        other,
                    )
                    .await
            }
            Material::HomeOAuth(_) => {
                let GooglePurpose::Dedicated { credential_ref, .. } =
                    &self.material.context.purpose
                else {
                    panic!("expected native purpose");
                };
                self.borrow_home_native(expected.tool(), expected.digest(), credential_ref)
                    .unwrap()
                    .test_foreign_native_step(expected, other)
                    .await
            }
        }
    }
}
