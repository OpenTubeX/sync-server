use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{LazyLock, Mutex},
    task::{Context, Poll, Waker},
};

struct WaiterState {
    device: String,
    waker: Option<Waker>,
}

static WAITERS: LazyLock<Mutex<HashMap<String, HashMap<String, WaiterState>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

pub struct ChangeWaiter {
    account: String,
    id: String,
}
impl ChangeWaiter {
    pub fn new(account: &str, device: &str) -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        WAITERS
            .lock()
            .unwrap()
            .entry(account.into())
            .or_default()
            .insert(
                id.clone(),
                WaiterState {
                    device: device.into(),
                    waker: None,
                },
            );
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
            slot.waker = Some(cx.waker().clone());
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
pub fn notify(account: &str, recipient: Option<&str>) {
    let mut ready = Vec::new();
    {
        let mut waiters = WAITERS.lock().unwrap();
        if let Some(entries) = waiters.get_mut(account) {
            entries.retain(|_, entry| {
                if recipient.is_none_or(|device| device == entry.device) {
                    if let Some(waker) = entry.waker.take() {
                        ready.push(waker);
                    }
                    false
                } else {
                    true
                }
            });
            if entries.is_empty() {
                waiters.remove(account);
            }
        }
    }
    // Wake outside the mutex so polling cannot reenter it while locked.
    for waker in ready {
        waker.wake();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[actix_rt::test]
    async fn change_before_poll_is_not_lost_and_accounts_are_isolated() {
        let a = ChangeWaiter::new("a", "phone");
        let b = ChangeWaiter::new("b", "phone");
        notify("a", None);
        actix_web::rt::time::timeout(std::time::Duration::from_millis(50), a)
            .await
            .unwrap();
        assert!(
            actix_web::rt::time::timeout(std::time::Duration::from_millis(5), b)
                .await
                .is_err()
        );
        assert!(!WAITERS.lock().unwrap().contains_key("a"));
        assert!(!WAITERS.lock().unwrap().contains_key("b"));
    }
    #[actix_rt::test]
    async fn device_request_does_not_wake_another_device() {
        let phone = ChangeWaiter::new("targeted", "phone");
        let laptop = ChangeWaiter::new("targeted", "laptop");
        notify("targeted", Some("phone"));
        actix_web::rt::time::timeout(std::time::Duration::from_millis(50), phone)
            .await
            .unwrap();
        assert!(
            actix_web::rt::time::timeout(std::time::Duration::from_millis(5), laptop)
                .await
                .is_err()
        );
    }
}
