//! Instance-owned cleanup, independent of host signals or stdin.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

type HookFuture = Pin<Box<dyn Future<Output = ()> + Send>>;
type Hook = Box<dyn FnOnce() -> HookFuture + Send>;

#[derive(Default)]
pub struct StopHooks {
    hooks: Vec<(String, Hook)>,
}

impl StopHooks {
    pub fn add<F, Fut>(&mut self, label: impl Into<String>, hook: F)
    where
        F: FnOnce() -> Fut + Send + 'static,
        Fut: Future<Output = ()> + Send + 'static,
    {
        self.hooks
            .push((label.into(), Box::new(move || Box::pin(hook()))));
    }

    pub async fn run(self, timeout: Duration) -> Vec<String> {
        let mut errors = Vec::new();
        for (label, hook) in self.hooks.into_iter().rev() {
            tracing::debug!("stopping {label}");
            if tokio::time::timeout(timeout, hook()).await.is_err() {
                let message = format!("{label} did not stop within {} seconds", timeout.as_secs());
                tracing::warn!("{message}");
                errors.push(message);
            }
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[tokio::test]
    async fn hooks_run_newest_first_and_report_incomplete_cleanup() {
        let order = Arc::new(Mutex::new(Vec::new()));
        let mut hooks = StopHooks::default();
        for label in ["backend", "stuck", "vite"] {
            let order = Arc::clone(&order);
            hooks.add(label, move || async move {
                if label == "stuck" {
                    std::future::pending::<()>().await;
                }
                order.lock().unwrap().push(label);
            });
        }
        let errors = hooks.run(Duration::from_millis(50)).await;
        assert_eq!(*order.lock().unwrap(), ["vite", "backend"]);
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("stuck did not stop"));
    }
}
