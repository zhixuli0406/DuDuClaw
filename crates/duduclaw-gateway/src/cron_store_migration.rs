//! Migration shares the ordinary cron row writer. Its receipt and authority
//! mutations commit in one transaction; normal edits invalidate the receipt.
use super::*;
impl CronStore {
    pub(crate) async fn migration_publish(&self,row:&CronTaskRow,operation:&str,digest:&str)->Result<(),String> {
        let mut conn=self.conn.lock().await;verify_io()?;
        init_receipts(&conn)?;
        let tx=begin_write(&mut conn)?;
        let receipt:Option<(String,String)>=tx.query_row("SELECT operation,digest FROM cron_migration_receipts WHERE id=?1",[&row.id],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(|e|e.to_string())?;
        if let Some(receipt)=receipt {
            if receipt!=(operation.into(),digest.into()) || !matches_row(&tx,row,false)? {return Err("migration schedule changed".into());}
            tx.execute("UPDATE cron_tasks SET enabled=1 WHERE id=?1",[&row.id]).map_err(|e|e.to_string())?;
        } else {
            insert_cron_row(&tx,row)?;
        }
        tx.execute("INSERT OR REPLACE INTO cron_migration_receipts VALUES(?1,?2,?3)",params![row.id,operation,digest]).map_err(|e|e.to_string())?;
        commit_write(tx)
    }
    pub(crate) async fn migration_verify(&self,row:&CronTaskRow,operation:&str,digest:&str,enabled:bool)->Result<(),String> {
        let conn=self.conn.lock().await;verify_io()?;init_receipts(&conn)?;
        let valid:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM cron_migration_receipts WHERE id=?1 AND operation=?2 AND digest=?3)",params![row.id,operation,digest],|r|r.get(0)).map_err(|e|e.to_string())?;
        let actual:bool=conn.query_row("SELECT enabled FROM cron_tasks WHERE id=?1",[&row.id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid || actual!=enabled || !matches_row(&conn,row,false)? {return Err("migration schedule readback changed".into());}Ok(())
    }
    pub(crate) async fn migration_disable(&self,row:&CronTaskRow,operation:&str,digest:&str)->Result<(),String> {
        let mut conn=self.conn.lock().await;verify_io()?;init_receipts(&conn)?;
        let tx=begin_write(&mut conn)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cron_tasks WHERE id=?1)",[&row.id],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !exists {return Ok(());}
        let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM cron_migration_receipts WHERE id=?1 AND operation=?2 AND digest=?3)",params![row.id,operation,digest],|r|r.get(0)).map_err(|e|e.to_string())?;
        if !valid || !matches_row(&tx,row,false)? {return Err("migration schedule compensation changed".into());}
        tx.execute("UPDATE cron_tasks SET enabled=0 WHERE id=?1",[&row.id]).map_err(|e|e.to_string())?;
        tx.execute("INSERT INTO cron_migration_receipts VALUES(?1,?2,?3)",params![row.id,operation,digest]).map_err(|e|e.to_string())?;
        commit_write(tx)
    }
}
fn init_receipts(conn:&Connection)->Result<(),String> {
    verify_io()?;
    conn.execute_batch("CREATE TABLE IF NOT EXISTS cron_migration_receipts(id TEXT PRIMARY KEY,operation TEXT NOT NULL,digest TEXT NOT NULL);
    CREATE TRIGGER IF NOT EXISTS cron_migration_edit AFTER UPDATE OF name,agent_id,cron,task,enabled,notify_channel,notify_chat_id,notify_thread_id,cron_timezone,trigger_kind,condition_script,watch_command,on_behalf_of,created_by_user_id,scoped_by_user_id ON cron_tasks BEGIN DELETE FROM cron_migration_receipts WHERE id=OLD.id; END;
    CREATE TRIGGER IF NOT EXISTS cron_migration_delete AFTER DELETE ON cron_tasks BEGIN DELETE FROM cron_migration_receipts WHERE id=OLD.id; END;").map_err(|e|e.to_string())
}
fn matches_row(conn:&Connection,row:&CronTaskRow,check_enabled:bool)->Result<bool,String> {
    conn.query_row("SELECT name=?2 AND agent_id=?3 AND cron=?4 AND task=?5 AND (NOT ?6 OR enabled=?7) AND created_at=?8 AND cron_timezone IS ?9 AND on_behalf_of IS ?10 AND trigger_kind='time' AND condition_script IS NULL AND watch_command IS NULL AND notify_channel IS NULL AND notify_chat_id IS NULL AND notify_thread_id IS NULL AND created_by_user_id IS ?11 AND scoped_by_user_id IS ?12 FROM cron_tasks WHERE id=?1",
        params![row.id,row.name,row.agent_id,row.cron,row.task,check_enabled,row.enabled,row.created_at,row.cron_timezone,row.on_behalf_of,row.created_by_user_id,row.scoped_by_user_id],|r|r.get(0)).map_err(|e|e.to_string())
}
#[cfg(test)] mod tests {
    use super::*;
    #[tokio::test] async fn migration_authority_cron_receipt_atomic_retry_and_normal_edit_fence() {
        let temp=tempfile::tempdir().unwrap();let store=CronStore::open(temp.path()).unwrap();
        let row=CronTaskRow::new("migration-test".into(),"test".into(),"sales".into(),"0 0 * * * *".into(),"write notes".into());
        assert!(store.list_enabled().await.unwrap().is_empty());
        store.migration_publish(&row,"op","digest").await.unwrap();store.migration_publish(&row,"op","digest").await.unwrap();store.migration_verify(&row,"op","digest",true).await.unwrap();
        store.migration_disable(&row,"op","digest").await.unwrap();store.migration_verify(&row,"op","digest",false).await.unwrap();store.migration_publish(&row,"op","digest").await.unwrap();
        // Even an equivalent normal edit changes ownership; compensation cannot
        // revoke an independently accepted schedule.
        store.set_enabled(&row.id,true).await.unwrap();assert!(store.migration_disable(&row,"op","digest").await.is_err());assert!(store.get(&row.id).await.unwrap().unwrap().enabled);
    }
}
