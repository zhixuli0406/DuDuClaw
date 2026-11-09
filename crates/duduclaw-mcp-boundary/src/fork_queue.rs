//! Bounded host queue. Never await a same-employee child inside its caller run.
use std::{collections::BTreeMap, sync::{Arc,Mutex}, time::Duration};
use duduclaw_isolation::execution::{ExecutionBackend,RunHandle,RunOutcome,WaitOrigin,RunReader};
use crate::{RequestContext,VerificationError,execution::PolicyFuture,forks::{Branch,ForkStore,ForkPlanFactory,Manifest}};

/// Snapshot implementations must execute a bounded isolated helper after the
/// branch terminal observation. Returning a manifest is not live-parent promotion.
pub trait ForkResultSnapshot: Send + Sync {
    fn snapshot<'a>(&'a self,context:&'a RequestContext,branch:&'a Branch,run:RunHandle) -> PolicyFuture<'a,Manifest>;
}
struct Batch { context:RequestContext,fork:String,branches:Vec<String> }
pub struct ForkQueue {
    queues:Mutex<BTreeMap<[u8;16],tokio::sync::mpsc::Sender<Batch>>>,
    store:Arc<ForkStore>,backend:Arc<dyn ExecutionBackend>,factory:Arc<dyn ForkPlanFactory>,snapshots:Arc<dyn ForkResultSnapshot>,
}
impl ForkQueue {
    pub fn new(store:Arc<ForkStore>,backend:Arc<dyn ExecutionBackend>,factory:Arc<dyn ForkPlanFactory>,snapshots:Arc<dyn ForkResultSnapshot>) -> Self {
        Self { queues:Mutex::new(BTreeMap::new()),store,backend,factory,snapshots }
    }
    /// Fresh authorization precedes durable queuing. Batches serialize per
    /// employee; root D8 still owns admission against every other run class.
    pub async fn enqueue(&self,context:&RequestContext,fork:&str,branches:Vec<String>) -> Result<(),VerificationError> {
        if !(2..=4).contains(&branches.len()) { return Err(VerificationError::Invalid); }
        for id in &branches {
            let (store,ctx,fork,id)=(self.store.clone(),context.clone(),fork.to_owned(),id.clone());
            let branch=crate::authority::verify_blocking(move || store.branch(&ctx,&fork,&id)).await?;
            self.factory.authorize(context,&branch).await?;
        }
        let sender={
            let mut queues=self.queues.lock().map_err(|_| VerificationError::Unavailable)?;
            let key=context.record().employee.0;
            if !queues.contains_key(&key) {
                if queues.len()>=128 { return Err(VerificationError::Unavailable); }
                let (sender,mut receiver)=tokio::sync::mpsc::channel::<Batch>(8);
                let (store,backend,factory,snapshots)=(self.store.clone(),self.backend.clone(),self.factory.clone(),self.snapshots.clone());
                tokio::spawn(async move {
                    while let Some(batch)=receiver.recv().await {
                        for id in batch.branches {
                            let completed=run_branch(store.clone(),backend.clone(),factory.as_ref(),snapshots.as_ref(),&batch.context,&batch.fork,&id).await;
                            if completed.is_err() {
                                let (store,fork,id)=(store.clone(),batch.fork.clone(),id.clone());
                                let _=crate::authority::verify_blocking(move || store.uncertain(&fork,&id)).await;
                            }
                        }
                    }
                });
                queues.insert(key,sender);
            }
            queues.get(&key).expect("inserted sender").clone()
        };
        // Reserve bounded queue capacity before changing durable state. No D8
        // admission is held by this host channel reservation.
        let permit=sender.try_reserve_owned().map_err(|_| VerificationError::Unavailable)?;
        let (store,ctx,fork_id,ids)=(self.store.clone(),context.clone(),fork.to_owned(),branches.clone());
        let batch=Batch { context:context.clone(),fork:fork.to_owned(),branches };
        crate::authority::verify_blocking(move || {
            store.queue(&ctx,&fork_id,&ids)?;
            // A cancelled JoinHandle cannot discard a committed queued batch.
            permit.send(batch);
            Ok(())
        }).await?;
        Ok(())
    }
}
async fn drain(mut reader:Box<dyn RunReader>) -> Result<(),VerificationError> {
    let mut buffer=[0;8192]; let mut total=0usize;
    loop {
        let n=reader.read(&mut buffer).await.map_err(|_| VerificationError::Unavailable)?;
        if n==0 { return Ok(()); }
        total=total.checked_add(n).ok_or(VerificationError::Invalid)?;
        if n>buffer.len() || total>1024*1024 { return Err(VerificationError::Invalid); }
    }
}
struct ActiveBranch {
    backend:Arc<dyn ExecutionBackend>,store:Arc<ForkStore>,run:RunHandle,fork:String,branch:String,armed:bool,
}
impl Drop for ActiveBranch {
    fn drop(&mut self) {
        if !self.armed { return; }
        let (backend,store,run,fork,branch)=(self.backend.clone(),self.store.clone(),self.run,self.fork.clone(),self.branch.clone());
        if let Ok(runtime)=tokio::runtime::Handle::try_current() {
            runtime.spawn(async move {
                let _=tokio::time::timeout(Duration::from_secs(5),backend.kill(run)).await;
                let _=crate::authority::verify_blocking(move || store.uncertain(&fork,&branch)).await;
            });
        }
    }
}
async fn run_branch(store:Arc<ForkStore>,backend:Arc<dyn ExecutionBackend>,factory:&dyn ForkPlanFactory,
    snapshots:&dyn ForkResultSnapshot,context:&RequestContext,fork:&str,branch:&str) -> Result<(),VerificationError> {
    let run=crate::forks::launch_branch(store.clone(),backend.clone(),factory,context,fork,branch).await?;
    let mut guard=ActiveBranch { backend:backend.clone(),store:store.clone(),run,fork:fork.to_owned(),branch:branch.to_owned(),armed:true };
    let observed=async {
        let mut io=loop {
            match backend.take_stdio(run).await {
                Ok(io)=>break io,
                Err(duduclaw_isolation::execution::BackendError::Unavailable)=>tokio::time::sleep(Duration::from_millis(50)).await,
                Err(_)=>return Err(VerificationError::Unavailable),
            }
        };
        let starting=store.clone();
        crate::authority::verify_blocking(move || starting.started(run)).await?;
        io.stdin.close().await.map_err(|_| VerificationError::Unavailable)?;
        let (out,err,outcome)=tokio::join!(drain(io.stdout),drain(io.stderr),backend.wait(run,WaitOrigin::Gateway));
        out?; err?; outcome.map_err(|_| VerificationError::Unavailable)
    };
    let outcome=match tokio::time::timeout(Duration::from_secs(1200),observed).await {
        Ok(Ok(outcome))=>outcome,
        _=>{ let _=tokio::time::timeout(Duration::from_secs(5),backend.kill(run)).await; return Err(VerificationError::Unavailable); }
    };
    let (checking,ctx,fork_id,branch_id)=(store.clone(),context.clone(),fork.to_owned(),branch.to_owned());
    let branch=crate::authority::verify_blocking(move || checking.branch(&ctx,&fork_id,&branch_id)).await?;
    let manifest=if outcome==RunOutcome::Exited(0) && branch.state!="cancel_requested" {
        Some(snapshots.snapshot(context,&branch,run).await?)
    } else { None };
    crate::authority::verify_blocking(move || store.finish(run,&outcome,manifest.as_ref())).await?;
    guard.armed=false;
    Ok(())
}
