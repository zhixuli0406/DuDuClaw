//! The editable status message of one messaging-app turn: the surface
//! trait every platform implements and the slot that tracks the posted
//! message (with a give-up rule, so a platform that refuses the message is
//! not asked again every few seconds).

use async_trait::async_trait;

use super::channel_format::StatusView;

/// Posting stops for the turn after this many consecutive failures.
pub const MAX_POST_FAILURES: u8 = 3;

/// What one `post` did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PostOutcome {
    /// Posted; the message id for later edits.
    Posted(String),
    /// The platform may have created the message but its id could not be
    /// read: never post again this turn (that would duplicate it).
    Unknown,
    /// Not posted (refused, network error).
    Failed,
}

/// One editable status message on a messaging platform.
#[async_trait]
pub trait StatusSurface: Send + Sync {
    /// Post a new status message.
    async fn post(&self, view: &StatusView) -> PostOutcome;
    /// Replace the status message's content.
    async fn edit(&self, id: &str, view: &StatusView);
    /// Remove the status message.
    async fn delete(&self, id: &str);
}

/// The posted message of one turn.
#[derive(Debug, Default)]
pub struct StatusSlot {
    pub id: Option<String>,
    failures: u8,
    /// Nothing more is posted this turn.
    dead: bool,
}

impl StatusSlot {
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Post `view` (when nothing is posted yet) or edit to it when
    /// `changed` (the view differs from the last one sent).
    pub async fn show(&mut self, surface: &dyn StatusSurface, view: &StatusView, changed: bool) {
        if self.dead {
            return;
        }
        match self.id.clone() {
            Some(id) if changed => surface.edit(&id, view).await,
            Some(_) => {}
            None => match surface.post(view).await {
                PostOutcome::Posted(id) => {
                    self.id = Some(id);
                    self.failures = 0;
                }
                PostOutcome::Unknown => self.dead = true,
                PostOutcome::Failed => {
                    self.failures += 1;
                    if self.failures >= MAX_POST_FAILURES {
                        self.dead = true;
                    }
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trigger_route::texts::Lang;
    use std::sync::Mutex;

    struct Scripted {
        outcomes: Mutex<Vec<PostOutcome>>,
        calls: Mutex<Vec<&'static str>>,
    }

    #[async_trait]
    impl StatusSurface for Scripted {
        async fn post(&self, _view: &StatusView) -> PostOutcome {
            self.calls.lock().unwrap().push("post");
            let mut o = self.outcomes.lock().unwrap();
            if o.is_empty() { PostOutcome::Failed } else { o.remove(0) }
        }
        async fn edit(&self, _id: &str, _view: &StatusView) {
            self.calls.lock().unwrap().push("edit");
        }
        async fn delete(&self, _id: &str) {
            self.calls.lock().unwrap().push("delete");
        }
    }

    fn view() -> StatusView {
        StatusView {
            done: false,
            seconds: 1,
            line: None,
            summary: None,
            lang: Lang::ZhTw,
        }
    }

    fn scripted(outcomes: Vec<PostOutcome>) -> Scripted {
        Scripted {
            outcomes: Mutex::new(outcomes),
            calls: Mutex::new(Vec::new()),
        }
    }

    #[tokio::test]
    async fn three_failed_posts_end_posting_for_the_turn() {
        let s = scripted(vec![]);
        let mut slot = StatusSlot::default();
        for _ in 0..10 {
            slot.show(&s, &view(), true).await;
        }
        assert_eq!(s.calls.lock().unwrap().len(), MAX_POST_FAILURES as usize);
        assert!(slot.is_dead() && slot.id.is_none());
    }

    #[tokio::test]
    async fn an_unreadable_id_is_never_posted_twice() {
        let s = scripted(vec![PostOutcome::Unknown, PostOutcome::Posted("x".into())]);
        let mut slot = StatusSlot::default();
        for _ in 0..5 {
            slot.show(&s, &view(), true).await;
        }
        assert_eq!(*s.calls.lock().unwrap(), ["post"]);
        assert!(slot.is_dead());
    }

    #[tokio::test]
    async fn a_posted_message_is_edited_only_when_changed() {
        let s = scripted(vec![PostOutcome::Failed, PostOutcome::Posted("m".into())]);
        let mut slot = StatusSlot::default();
        slot.show(&s, &view(), true).await;
        slot.show(&s, &view(), true).await;
        slot.show(&s, &view(), false).await;
        slot.show(&s, &view(), true).await;
        assert_eq!(*s.calls.lock().unwrap(), ["post", "post", "edit"]);
        assert_eq!(slot.id.as_deref(), Some("m"));
    }
}
