//! Fresh channel routing bindings. No old bind tokens or channel authentication
//! state is restored. The ordinary inbound router reads this store per message.
use super::*;
#[derive(Serialize,Deserialize)]
pub(super) struct ChannelPlan {operation:String,digest:String,channel:String,user:String,agent:String}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChannelInput {external_user_id:String}
pub(super) fn channel_plan(home:&Path,item:&ConfirmationItem)->Result<ChannelPlan> {
    let agent=text(item,"employee_id")?;let channel=text(item,"channel")?;
    if !duduclaw_core::is_valid_agent_id(agent) || channel!="telegram" {return Err(Error::Invalid);}
    let path=home.join("agents").join(agent).join("agent.toml");validate_parents(home,&path)?;regular(&path)?;
    let input:ChannelInput=serde_json::from_str(text(item,"binding")?).map_err(|_|Error::Invalid)?;
    // Telegram's stable numeric principal, not a display handle or reusable token.
    if input.external_user_id.is_empty() || input.external_user_id.len()>20 || !input.external_user_id.bytes().all(|b|b.is_ascii_digit()) {return Err(Error::Invalid);}
    Ok(ChannelPlan {operation:operation(&item.binding),digest:hex::encode(item.binding.proposal_digest.0),channel:channel.into(),user:input.external_user_id,agent:agent.into()})
}
impl ChannelPlan {
    fn store(&self,home:&Path)->crate::agent_binding::AgentBindingStore {crate::agent_binding::AgentBindingStore::with_persistence(home.join("agent_bindings.json"))}
    pub(super) fn publish(&self,home:&Path)->Result<()> {self.store(home).migration_bind(&self.operation,&self.digest,&self.channel,&self.user,&self.agent).map_err(|_|Error::Apply)}
    pub(super) fn read_back(&self,home:&Path,live:&super::categories::LiveAuthority)->Result<()> {
        let store=self.store(home);store.migration_readback(&self.operation,&self.digest,&self.channel,&self.user,&self.agent).map_err(|_|Error::Apply)?;
        if bounded(&live.handle,store.resolve_bound_agent(&self.channel,&self.user))?.as_deref()!=Some(self.agent.as_str()) {return Err(Error::Apply);}Ok(())
    }
    pub(super) fn disable(&self,home:&Path,live:&super::categories::LiveAuthority)->Result<()> {
        let store=self.store(home);store.migration_unbind(&self.operation,&self.digest).map_err(|_|Error::Compensation)?;
        if bounded(&live.handle,store.resolve_bound_agent(&self.channel,&self.user))?.is_some() {return Err(Error::Compensation);}Ok(())
    }
}
