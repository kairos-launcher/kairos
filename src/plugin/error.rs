//! Errors from plugins and the protocol that talks to them.
//!
//! Variants that change what the host does (retry, back off, keep the process) are typed, so
//! that logic matches on them instead of on message text. Everything else is a
//! [`Error::Message`] already worded for the user.

use serde_json::Value;

// Some variants only come from remote plugins.
#[cfg_attr(not(feature = "remote"), allow(dead_code))]
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Error {
    /// No reply in time. A stdio plugin is kept running: replies carry ids, so a late one is
    /// skipped by the next request.
    #[error("timeout")]
    Timeout,
    /// A newer query superseded this one.
    #[error("cancelled")]
    Cancelled,
    /// The plugin process exited or closed its output.
    #[error("plugin exited")]
    Exited,
    /// The plugin answered with a JSON-RPC `error` object: it is up, it just said no.
    #[error("{0}")]
    Rpc(Value),
    /// A remote plugin refused `initialize`.
    #[error("rejected: {0}")]
    Rejected(Value),
    /// HTTP 401 / 403 from a remote plugin.
    #[error("not authorized (HTTP {0}); check the API token")]
    Unauthorized(u16),
    /// Any other non-2xx HTTP status.
    #[error("HTTP {0}")]
    Http(u16),
    /// A remote plugin failed recently; requests are skipped until the back-off expires.
    #[error("unavailable (backing off after errors)")]
    BackingOff,
    /// A result asked for an action its plugin does not know (stale or forged ids).
    #[error("unknown action {0}")]
    UnknownAction(String),
    /// An action was asked for a result its plugin does not know.
    #[error("unknown result {0}")]
    UnknownResult(String),
    /// The plugin is disabled or was removed since the result was shown.
    #[error("plugin is disabled or was removed")]
    Gone,
    /// Context: which request failed.
    #[error("{method}: {source}")]
    Call {
        method: &'static str,
        #[source]
        source: Box<Error>,
    },
    /// Anything else, worded for the user.
    #[error("{0}")]
    Message(String),
}

#[cfg_attr(not(feature = "remote"), allow(dead_code))]
impl Error {
    /// Wrap with the name of the request that failed (`query: timeout`).
    pub fn during(self, method: &'static str) -> Self {
        Self::Call { method, source: Box::new(self) }
    }

    /// The error without its `Call` context, for matching on the cause.
    pub fn cause(&self) -> &Self {
        match self {
            Self::Call { source, .. } => source.cause(),
            other => other,
        }
    }

    pub fn is_timeout(&self) -> bool {
        matches!(self.cause(), Self::Timeout)
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self.cause(), Self::Cancelled)
    }

    /// The server is reachable and answered; this must not trigger a back-off.
    pub fn is_answer(&self) -> bool {
        matches!(self.cause(), Self::Rpc(_) | Self::Cancelled)
    }
}

impl From<String> for Error {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

impl From<&str> for Error {
    fn from(message: &str) -> Self {
        Self::Message(message.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_looks_through_context_not_at_text() {
        let e = Error::Timeout.during("query");
        assert_eq!(e.to_string(), "query: timeout");
        assert!(e.is_timeout());
        // A message that merely mentions a timeout is not one.
        assert!(!Error::Message("query: timeout here".into()).is_timeout());
        assert!(Error::Rpc(serde_json::json!({"code": 1})).during("execute").is_answer());
        assert!(!Error::Http(500).is_answer());
    }
}
