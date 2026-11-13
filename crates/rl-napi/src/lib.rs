//! Private JSON bridge. The npm wrapper owns the supported JavaScript API.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError, Weak};

use napi::{Env, Error, Result, Status};
use napi_derive::napi;
use rl_runtime::Runtime;

type Runtimes = Arc<Mutex<Vec<Weak<Runtime>>>>;

thread_local! {
    static ENVIRONMENTS: RefCell<HashMap<usize, Runtimes>> = RefCell::default();
}

fn track(env: Env, runtime: &Arc<Runtime>) -> Result<()> {
    let key = env.raw() as usize;
    ENVIRONMENTS.with(|environments| {
        let mut environments = environments.borrow_mut();
        if let std::collections::hash_map::Entry::Vacant(entry) = environments.entry(key) {
            let runtimes = Runtimes::default();
            env.add_env_cleanup_hook(runtimes.clone(), move |runtimes| {
                for runtime in runtimes
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .drain(..)
                {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.request_close();
                    }
                }
                ENVIRONMENTS.with(|environments| environments.borrow_mut().remove(&key));
            })?;
            entry.insert(runtimes);
        }
        let mut runtimes = environments[&key]
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        runtimes.retain(|runtime| runtime.strong_count() > 0);
        runtimes.push(Arc::downgrade(runtime));
        Ok(())
    })
}

#[napi]
pub struct NativeRuntime {
    runtime: Arc<Runtime>,
}

fn failure(error: rl_runtime::RuntimeError) -> Error {
    Error::new(
        Status::GenericFailure,
        serde_json::to_string(&error).expect("runtime errors serialize"),
    )
}

#[napi]
impl NativeRuntime {
    #[napi(constructor)]
    pub fn new(env: Env, options: String) -> Result<Self> {
        let options = serde_json::from_str(&options).map_err(|error| {
            Error::new(
                Status::InvalidArg,
                serde_json::json!({
                    "code": "config-invalid",
                    "message": format!("Invalid runtime options: {error}"),
                    "hint": null,
                    "details": {},
                })
                .to_string(),
            )
        })?;
        let runtime = Arc::new(Runtime::new(options).map_err(failure)?);
        track(env, &runtime)?;
        Ok(Self { runtime })
    }

    #[napi(getter)]
    pub fn state(&self) -> String {
        self.runtime.state().into()
    }

    #[napi]
    pub async fn start(&self) -> Result<String> {
        let ready = self.runtime.start().await.map_err(failure)?;
        serde_json::to_string(&ready).map_err(|error| Error::from_reason(error.to_string()))
    }

    #[napi]
    pub fn status(&self) -> String {
        self.runtime.status().to_string()
    }

    #[napi]
    pub async fn reload(&self, force_schema: bool) -> Result<String> {
        let result = self.runtime.reload(force_schema).await.map_err(failure)?;
        serde_json::to_string(&result).map_err(|error| Error::from_reason(error.to_string()))
    }

    #[napi]
    pub async fn close(&self) -> Result<String> {
        serde_json::to_string(&self.runtime.close().await)
            .map_err(|error| Error::from_reason(error.to_string()))
    }

    #[napi]
    pub fn request_close(&self) {
        self.runtime.request_close();
    }

    #[napi]
    pub fn drain_events(&self) -> String {
        serde_json::to_string(&self.runtime.drain_events()).expect("events serialize")
    }
}

#[napi]
pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}
