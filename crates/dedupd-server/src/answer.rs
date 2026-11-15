//! The answers the control API and Discover give: a value as JSON, or a
//! failure with why, and the status that fits it.

use crate::{Call, json};
use axum::http::StatusCode;
use axum::response::Response;
use dedupd_api::daemon::Failure;
use dedupd_core::db::NotInLibrary;
use dedupd_core::hub::HubError;
use serde::de::DeserializeOwned;

pub(crate) fn error(status: StatusCode, e: impl std::fmt::Display) -> Response {
    let failure = Failure {
        error: e.to_string(),
    };
    json(status, &serde_json::to_value(failure).unwrap_or_default())
}

pub(crate) fn internal(e: impl std::fmt::Display) -> Response {
    error(StatusCode::INTERNAL_SERVER_ERROR, e)
}

pub(crate) fn ok(value: impl serde::Serialize) -> Response {
    json(
        StatusCode::OK,
        &serde_json::to_value(value).unwrap_or_default(),
    )
}

pub(crate) fn answer<T: serde::Serialize>(result: anyhow::Result<T>) -> Response {
    match result {
        Ok(value) => ok(value),
        Err(e) => failure(e),
    }
}

/// A failure, with the status its kind calls for: a model the library
/// does not have is 404, the provider's failures are its own, anything
/// else is the daemon's.
pub(crate) fn failure(e: anyhow::Error) -> Response {
    if e.is::<NotInLibrary>() {
        return error(StatusCode::NOT_FOUND, e);
    }
    match e.downcast::<HubError>() {
        Ok(hub) => hub_error(hub),
        Err(e) => internal(format!("{e:#}")),
    }
}

/// What a handler answers instead of doing its work.
pub(crate) type Refused = Box<Response>;

pub(crate) fn body<T: DeserializeOwned>(call: &Call) -> Result<T, Refused> {
    serde_json::from_slice(&call.body).map_err(|e| Box::new(error(StatusCode::BAD_REQUEST, e)))
}

/// What the provider's failure means for someone who asked the daemon.
pub(crate) fn hub_error(e: HubError) -> Response {
    let status = match &e {
        HubError::NotAvailable { .. } => StatusCode::NOT_FOUND,
        HubError::Busy { .. } => StatusCode::TOO_MANY_REQUESTS,
        _ => StatusCode::BAD_GATEWAY,
    };
    error(status, e)
}

/// Run work that reads files or waits for the database off the threads that
/// answer requests.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(work).await?
}
