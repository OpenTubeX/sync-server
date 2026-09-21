use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{LazyLock, Mutex},
    task::{Context, Poll, Waker},
};

static WAITERS: LazyLock<Mutex<HashMap<String, HashMap<String, Option<Waker>>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct ChangeWaiter {
    account: String,
    id: String,
}
impl ChangeWaiter {
    pub fn new(account: &str) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        WAITERS
            .lock()
            .unwrap()
            .entry(account.into())
            .or_default()
            .insert(id.clone(), None);
        Self {
            account: account.into(),
            id,
        }
    }
}
impl Future for ChangeWaiter {
    type Output = ();
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<()> {
        let mut waiters = WAITERS.lock().unwrap();
        if let Some(slot) = waiters
            .get_mut(&self.account)
            .and_then(|entries| entries.get_mut(&self.id))
        {
            *slot = Some(cx.waker().clone());
            Poll::Pending
        } else {
            Poll::Ready(())
        }
    }
}
impl Drop for ChangeWaiter {
    fn drop(&mut self) {
        let mut waiters = WAITERS.lock().unwrap();
        if let Some(entries) = waiters.get_mut(&self.account) {
            entries.remove(&self.id);
            if entries.is_empty() {
                waiters.remove(&self.account);
            }
        }
    }
}
pub fn notify(account: &str) {
    let entries = WAITERS.lock().unwrap().remove(account).unwrap_or_default();
    for waker in entries.into_values().flatten() {
        waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[actix_rt::test]
    async fn change_before_poll_is_not_lost_and_accounts_are_isolated() {
        let a = ChangeWaiter::new("a");
        let b = ChangeWaiter::new("b");
        notify("a");
        actix_web::rt::time::timeout(std::time::Duration::from_millis(50), a)
            .await
            .unwrap();
        assert!(
            actix_web::rt::time::timeout(std::time::Duration::from_millis(5), b)
                .await
                .is_err()
        );
        assert!(WAITERS.lock().unwrap().is_empty());
    }
}
