//! CLI ownership: stdin, signals, the control secret, and process exit codes.

use std::ffi::OsString;
use std::io;
use std::process::ExitCode;
use std::sync::OnceLock;

use anyhow::anyhow;
use rl_runtime::commands::serve::{self as core, Running};

use crate::errors::HostError;
use crate::events::{self, StopReason, Stopped};
use crate::shutdown::{self, StopListener, StopRequest};
use crate::ui;

#[cfg(test)]
pub use core::clear_data;
pub use core::{ServeOptions, warn_lock_unavailable};

const INTERRUPTED: u8 = 130;
const CONTROL_TOKEN_VAR: &str = "RAYFIN_LOCAL_CONTROL_TOKEN";
static CONTROL_TOKEN: OnceLock<Option<OsString>> = OnceLock::new();

/// Called before threads start, so project children never inherit the control secret.
pub fn take_control_token() {
    let token = std::env::var_os(CONTROL_TOKEN_VAR);
    if token.is_some() {
        // SAFETY: main calls this before starting any thread.
        unsafe { std::env::remove_var(CONTROL_TOKEN_VAR) };
    }
    let _ = CONTROL_TOKEN.set(token);
}

enum Outcome {
    Stop(StopRequest),
    Failed(anyhow::Error),
    BackendExited(anyhow::Error),
}

pub async fn run(at: &super::Location, mut options: ServeOptions) -> anyhow::Result<ExitCode> {
    options.control_token = CONTROL_TOKEN
        .get()
        .and_then(Option::as_ref)
        .map(|token| token.to_string_lossy().into_owned());
    let mut running = Running::new();
    let mut ready = running.ready();
    let serve = async {
        match core::serve(at, &options, &mut running).await {
            Ok(core::Outcome::BackendExited(error)) => Outcome::BackendExited(error),
            Err(error) => Outcome::Failed(error),
        }
    };
    let outcome = if options.json {
        let mut stop = StopListener::with_input(io::stdin());
        tokio::select! {
            outcome = serve => outcome,
            request = stop.requested() => match request {
                Ok(request) => Outcome::Stop(request),
                Err(error) => Outcome::Failed(anyhow!(error).context("Could not listen for Ctrl+C")),
            },
        }
    } else {
        let signal = async {
            // Human-mode signal handling historically starts once the app is ready.
            if ready.wait_for(|ready| *ready).await.is_err() {
                std::future::pending::<()>().await;
            }
            shutdown::signal().await
        };
        tokio::select! {
            outcome = serve => outcome,
            request = signal => match request {
                Ok(()) => Outcome::Stop(StopRequest::Signal),
                Err(error) => Outcome::Failed(anyhow!(error).context("Could not listen for Ctrl+C")),
            },
        }
    };
    finish(outcome, running, options.json).await
}

async fn finish(outcome: Outcome, running: Running, json: bool) -> anyhow::Result<ExitCode> {
    match &outcome {
        Outcome::Stop(_) => {
            ui::status("Stopping…");
            tokio::spawn(async move {
                if shutdown::signal().await.is_ok() {
                    if json {
                        events::finish(Stopped {
                            reason: StopReason::Signal,
                            exit_code: INTERRUPTED,
                        });
                    } else {
                        eprintln!();
                    }
                    std::process::exit(i32::from(INTERRUPTED));
                }
            });
        }
        Outcome::Failed(error) | Outcome::BackendExited(error) if json => {
            events::emit(HostError::classify(error).event(true));
        }
        Outcome::Failed(_) | Outcome::BackendExited(_) => {}
    }
    running.shut_down().await;
    let (reason, code) = match outcome {
        Outcome::Stop(request) => {
            ui::status("Stopped.");
            let reason = match request {
                StopRequest::Signal => StopReason::Signal,
                StopRequest::StdinClosed => StopReason::StdinClosed,
            };
            (reason, if json { 0 } else { INTERRUPTED })
        }
        Outcome::Failed(error) | Outcome::BackendExited(error) if !json => return Err(error),
        Outcome::Failed(_) => (StopReason::Error, 1),
        Outcome::BackendExited(_) => (StopReason::BackendExited, 1),
    };
    if json {
        events::finish(Stopped {
            reason,
            exit_code: code,
        });
    }
    Ok(ExitCode::from(code))
}
