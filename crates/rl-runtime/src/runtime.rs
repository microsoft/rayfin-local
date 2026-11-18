//! Instance-owned, in-process lifecycle. No signal handlers, stdin readers, or global logger.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::watch;
use tracing::instrument::WithSubscriber as _;
use tracing_subscriber::layer::SubscriberExt as _;

use crate::commands::{Location, serve};
use crate::errors::HostError;
use crate::events::{self, EventContext};
use crate::reload::ReloadHandle;

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RuntimeOptions {
    pub project_dir: PathBuf,
    pub state_dir: Option<PathBuf>,
    pub node_path: Option<PathBuf>,
    #[serde(default)]
    pub server: ServerOptions,
    #[serde(default)]
    pub frontend: FrontendOptions,
    #[serde(default)]
    pub functions: FunctionsOptions,
    #[serde(default)]
    pub force_schema: bool,
    #[serde(default)]
    pub seed: bool,
    #[serde(default)]
    pub verbose: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerOptions {
    #[serde(default = "loopback")]
    pub host: String,
    #[serde(default)]
    pub port: u16,
}

fn loopback() -> String {
    "127.0.0.1".into()
}

impl Default for ServerOptions {
    fn default() -> Self {
        Self {
            host: loopback(),
            port: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FrontendOptions {
    #[serde(default)]
    pub mode: FrontendMode,
    pub port: Option<u16>,
    #[serde(default)]
    pub reuse_build: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FrontendMode {
    #[default]
    Dev,
    Static,
    Off,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FunctionsOptions {
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl Default for FunctionsOptions {
    fn default() -> Self {
        Self { enabled: true }
    }
}

#[derive(Clone, Debug, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct RuntimeError {
    pub code: String,
    pub message: String,
    pub hint: Option<String>,
    pub details: Box<Value>,
}

impl RuntimeError {
    pub(crate) fn from_error(error: &anyhow::Error) -> Self {
        let error = HostError::classify(error);
        Self {
            code: serde_json::to_value(error.code)
                .expect("error codes serialize")
                .as_str()
                .expect("string code")
                .into(),
            message: error.message,
            hint: error.hint,
            details: Box::new(error.details),
        }
    }

    fn lifecycle(code: &str, message: &str) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            hint: None,
            details: Box::new(json!({})),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StopInfo {
    pub reason: String,
    pub error: Option<RuntimeError>,
    pub cleanup_errors: Vec<String>,
}

#[derive(Clone)]
struct Progress {
    state: &'static str,
    stopped: Option<StopInfo>,
}

pub(crate) struct Shared {
    progress: watch::Sender<Progress>,
    stop: watch::Sender<bool>,
    reload: Mutex<Option<ReloadHandle>>,
    events: EventContext,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    pub(crate) fn set_reload(&self, reload: ReloadHandle) {
        *lock(&self.reload) = Some(reload);
    }

    pub(crate) fn stopping(&self) {
        self.progress
            .send_modify(|progress| progress.state = "stopping");
        lock(&self.reload).take();
    }

    fn finish(&self, info: StopInfo) {
        self.progress.send_modify(|progress| {
            progress.state = "stopped";
            progress.stopped = Some(info);
        });
    }
}

/// A single-use runtime. Dropping the owner requests shutdown; use `close` to await it.
pub struct Runtime {
    options: RuntimeOptions,
    shared: Arc<Shared>,
}

impl Runtime {
    /// Validates options without touching the project, host environment, or network.
    pub fn new(options: RuntimeOptions) -> Result<Self, RuntimeError> {
        if !options.project_dir.is_absolute()
            || options
                .state_dir
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
            || options
                .node_path
                .as_ref()
                .is_some_and(|path| !path.is_absolute())
        {
            return Err(RuntimeError::lifecycle(
                "config-invalid",
                "projectDir, stateDir, and nodePath must be absolute paths.",
            ));
        }
        if options.server.host.trim().is_empty() {
            return Err(RuntimeError::lifecycle(
                "config-invalid",
                "server.host must not be empty.",
            ));
        }
        if options.frontend.port == Some(0) && matches!(options.frontend.mode, FrontendMode::Dev) {
            return Err(RuntimeError::lifecycle(
                "config-invalid",
                "frontend.port must be nonzero; omit it to choose an available port.",
            ));
        }
        Ok(Self {
            options,
            shared: Arc::new(Shared {
                progress: watch::channel(Progress {
                    state: "created",
                    stopped: None,
                })
                .0,
                stop: watch::channel(false).0,
                reload: Mutex::default(),
                events: EventContext::new(),
            }),
        })
    }

    pub fn state(&self) -> &'static str {
        let progress = self.shared.progress.borrow();
        if progress.state == "starting" && self.shared.events.live().snapshot().ready.is_some() {
            "running"
        } else {
            progress.state
        }
    }

    pub fn status(&self) -> Value {
        let mut status = self
            .shared
            .events
            .live()
            .status()
            .unwrap_or_else(|| json!({}));
        status["state"] = self.state().into();
        status
    }

    pub fn drain_events(&self) -> Vec<Value> {
        self.shared.events.drain()
    }

    pub async fn start(&self) -> Result<Value, RuntimeError> {
        let mut launch = false;
        self.shared.progress.send_modify(|progress| {
            if progress.state == "created" {
                progress.state = "starting";
                launch = true;
            }
        });
        if launch {
            self.launch();
        }
        let mut progress_rx = self.shared.progress.subscribe();
        let mut changed = self.shared.events.subscribe();
        loop {
            let progress = progress_rx.borrow_and_update().clone();
            if let Some(info) = progress.stopped {
                return Err(info.error.unwrap_or_else(|| {
                    RuntimeError::lifecycle("aborted", "Runtime stopped before startup completed.")
                }));
            }
            if progress.state != "stopping"
                && let Some(ready) = self.shared.events.live().snapshot().ready
            {
                return Ok(ready);
            }
            tokio::select! {
                _ = changed.changed() => {}
                _ = progress_rx.changed() => {}
            }
        }
    }

    fn launch(&self) {
        let shared = self.shared.clone();
        let location = Location {
            dir: self.options.project_dir.clone(),
            state_dir: self.options.state_dir.clone(),
        };
        let options = serve::ServeOptions {
            json: true,
            port: Some(self.options.server.port),
            host: Some(self.options.server.host.clone()),
            frontend_port: self.options.frontend.port,
            static_build: matches!(self.options.frontend.mode, FrontendMode::Static),
            reuse_build: self.options.frontend.reuse_build,
            no_frontend: matches!(self.options.frontend.mode, FrontendMode::Off),
            no_functions: !self.options.functions.enabled,
            force_schema: self.options.force_schema,
            seed: self.options.seed,
            verbose: self.options.verbose,
            node_path: self.options.node_path.clone(),
            ..Default::default()
        };
        let subscriber = tracing_subscriber::registry().with(events::LogLayer {
            verbose: options.verbose,
        });
        let context = shared.events.clone();
        let stop = shared.stop.subscribe();
        let task_shared = shared.clone();
        let task = tokio::spawn(
            async move {
                context
                    .scope(serve::run_embedded(&location, &options, &task_shared, stop))
                    .await
            }
            .with_subscriber(subscriber),
        );
        tokio::spawn(async move {
            let info = match task.await {
                Ok(info) => info,
                Err(error) => StopInfo {
                    reason: "error".into(),
                    error: Some(RuntimeError::lifecycle(
                        "internal",
                        &format!("Runtime task failed: {error}"),
                    )),
                    cleanup_errors: vec![
                        "Runtime task failed unexpectedly; cleanup may be incomplete.".into(),
                    ],
                },
            };
            shared.finish(info);
        });
    }

    pub async fn reload(
        &self,
        force_schema: bool,
    ) -> Result<crate::reload::ReloadOutcome, RuntimeError> {
        if self.state() != "running" {
            return Err(RuntimeError::lifecycle(
                "not-running",
                "Start the runtime before reloading it.",
            ));
        }
        let reload = lock(&self.shared.reload)
            .clone()
            .ok_or_else(|| RuntimeError::lifecycle("not-running", "Runtime is stopping."))?;
        reload
            .reload(force_schema)
            .await
            .map_err(|error| RuntimeError::from_error(&error))
    }

    pub fn request_close(&self) {
        let mut never_started = false;
        self.shared.progress.send_modify(|progress| {
            never_started = progress.state == "created";
            if progress.state != "stopped" {
                progress.state = "stopping";
            }
        });
        self.shared.stop.send_replace(true);
        if never_started {
            self.shared.finish(StopInfo {
                reason: "closed".into(),
                error: None,
                cleanup_errors: vec![],
            });
        }
    }

    pub async fn close(&self) -> StopInfo {
        self.request_close();
        self.closed().await
    }

    pub async fn closed(&self) -> StopInfo {
        let mut progress = self.shared.progress.subscribe();
        loop {
            if let Some(info) = progress.borrow_and_update().stopped.clone() {
                return info;
            }
            progress
                .changed()
                .await
                .expect("runtime owns progress sender");
        }
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.request_close();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options(project_dir: PathBuf) -> RuntimeOptions {
        serde_json::from_value(json!({
            "projectDir": project_dir,
            "frontend": {"mode": "off"},
            "functions": {"enabled": false},
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn concurrent_start_failure_settles_all_waiters_and_close() {
        let temporary = tempfile::tempdir().unwrap();
        let runtime = Runtime::new(options(temporary.path().to_owned())).unwrap();
        let (first, second) = tokio::join!(runtime.start(), runtime.start());
        assert_eq!(first.unwrap_err().code, "project-not-found");
        assert_eq!(second.unwrap_err().code, "project-not-found");
        let stopped = runtime.close().await;
        assert_eq!(stopped.error.unwrap().code, "project-not-found");
        assert!(stopped.cleanup_errors.is_empty());
        assert_eq!(runtime.state(), "stopped");
        assert_eq!(runtime.drain_events().last().unwrap()["type"], "stopped");
    }

    #[tokio::test]
    async fn closing_before_start_is_terminal_and_does_not_discover_project() {
        let runtime = Runtime::new(options(
            std::env::current_dir().unwrap().join("not-a-project"),
        ))
        .unwrap();
        assert_eq!(runtime.state(), "created");
        assert_eq!(runtime.reload(false).await.unwrap_err().code, "not-running");
        assert_eq!(runtime.close().await.reason, "closed");
        assert_eq!(runtime.close().await.reason, "closed");
        assert!(runtime.drain_events().is_empty());
        assert!(runtime.start().await.is_err());
    }
}
