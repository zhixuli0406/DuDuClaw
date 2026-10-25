//! Invoke the actual direct-Claude producer with a fixture future, never a
//! model. Background classification must precede context minting on this rail.
use super::*;
use crate::{computer_use_sessions::attended,cost_telemetry::RequestType};

async fn check(kind:RequestType) {
    let home=tempfile::tempdir().unwrap();
    let agent=format!("caller-producer-{}",kind.as_str());
    let session=format!("line:producer-{}",kind.as_str());
    attended::verified_channel_message(&session,"human",false,async {
        assert!(attended::bind_final_route(&session,&session,&agent,"real-producer-turn"));
        duduclaw_memory::feedback::CURRENT_SESSION_ID.scope(Some(session.clone()),
            duduclaw_memory::feedback::CURRENT_TURN_ID.scope(Some("real-producer-turn".into()),
                crate::on_behalf_of::ON_BEHALF_OF.scope(Some("user:owner".into()),async {
                    let result=invoke_recorded(home.path(),&agent,"synthetic request",kind,async {
                        let (_,reference)=crate::on_behalf_of::context::env_pair().expect("real producer owns a context");
                        assert_eq!(crate::on_behalf_of::context::lookup(home.path(),&agent,&reference),Some(Some("user:owner".into())));
                        let proof=crate::on_behalf_of::context::attended_call(home.path(),&agent,&reference);
                        assert_eq!(proof.is_some(),kind==RequestType::Chat,"actual invoke_recorded producer: {kind}");
                        // Empty success skips post-run ingestion and every model/provider call.
                        Ok(String::new())
                    }).await;
                    assert!(result.is_ok());
                }))).await;
    }).await;
}
#[tokio::test]
async fn attended_invoke_recorded_chat_keeps_the_live_caller() {check(RequestType::Chat).await;}
#[tokio::test]
async fn attended_invoke_recorded_evolution_refuses_inherited_human_scope() {check(RequestType::Evolution).await;}
#[tokio::test]
async fn attended_invoke_recorded_dispatch_refuses_inherited_human_scope() {check(RequestType::Dispatch).await;}
#[tokio::test]
async fn attended_invoke_recorded_cron_refuses_inherited_human_scope() {check(RequestType::Cron).await;}
