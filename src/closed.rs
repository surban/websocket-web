use std::pin::Pin;
use std::task::{Context, Poll};
use std::{fmt, io};
use std::future::Future;

use futures::FutureExt;


/// Reason for why a WebSocket connection is closed.
#[derive(Debug, Clone)]
pub struct ClosedReason {
    /// A number representing the closing code.
    pub code: CloseCode,
    /// A string representing a human-readable description of
    /// the reason why the socket connection was closed.
    pub reason: String,
}

impl fmt::Display for ClosedReason {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> fmt::Result {
        if self.reason.is_empty() {
            write!(f, "{}", self.code)
        } else {
            write!(f, "{} ({})", &self.reason, self.code)
        }
    }
}

/// A close code indicating why a WebSocket connection was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u16)]
pub enum CloseCode {
    /// The connection successfully completed the purpose for which it was created.
    NormalClosure = 1000,
    /// The endpoint is going away, either because of a server failure or a navigation away.
    GoingAway = 1001,
    /// The endpoint is terminating the connection due to a protocol error.
    ProtocolError = 1002,
    /// The connection is being terminated because the endpoint received data of a type it cannot accept.
    UnsupportedData = 1003,
    /// Reserved: Indicates that no status code was provided although one was expected.
    NoStatusRcvd = 1005,
    /// Reserved: Indicates that a connection was closed abnormally when a status code was expected.
    AbnormalClosure = 1006,
    /// The endpoint is terminating the connection because a message has inconsistent data.
    InvalidFramePayloadData = 1007,
    /// The endpoint is terminating the connection because it received a message that violates its policy.
    PolicyViolation = 1008,
    /// The endpoint is terminating the connection because a data frame was received that is too large.
    MessageTooBig = 1009,
    /// The client is terminating the connection because it expected a server extension negotiation.
    MandatoryExt = 1010,
    /// The server is terminating the connection because it encountered an unexpected condition.
    InternalError = 1011,
    /// The server is terminating the connection because it is restarting.
    ServiceRestart = 1012,
    /// The server is terminating the connection due to a temporary condition (e.g., overloaded).
    TryAgainLater = 1013,
    /// The server was acting as a gateway or proxy and received an invalid response from upstream.
    BadGateway = 1014,
    /// Reserved: The connection was closed due to a failure in the TLS handshake.
    TlsHandshake = 1015,
    /// Other close code.
    Other(u16),
}

impl fmt::Display for CloseCode {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            CloseCode::NormalClosure => write!(f, "normal closure"),
            CloseCode::GoingAway => write!(f, "going away"),
            CloseCode::ProtocolError => write!(f, "protocol error"),
            CloseCode::UnsupportedData => write!(f, "unsupported data"),
            CloseCode::NoStatusRcvd => write!(f, "no status rcvd"),
            CloseCode::AbnormalClosure => write!(f, "abnormal closure"),
            CloseCode::InvalidFramePayloadData => write!(f, "invalid frame payload data"),
            CloseCode::PolicyViolation => write!(f, "policy violation"),
            CloseCode::MessageTooBig => write!(f, "message too big"),
            CloseCode::MandatoryExt => write!(f, "mandatory ext"),
            CloseCode::InternalError => write!(f, "internal error"),
            CloseCode::ServiceRestart => write!(f, "service restart"),
            CloseCode::TryAgainLater => write!(f, "try again later"),
            CloseCode::BadGateway => write!(f, "bad gateway"),
            CloseCode::TlsHandshake => write!(f, "TLS handshake"),
            CloseCode::Other(code) => write!(f, "{code}"),
        }
    }
}

impl From<CloseCode> for u16 {
    fn from(code: CloseCode) -> Self {
        match code {
            CloseCode::NormalClosure => 1000,
            CloseCode::GoingAway => 1001,
            CloseCode::ProtocolError => 1002,
            CloseCode::UnsupportedData => 1003,
            CloseCode::NoStatusRcvd => 1005,
            CloseCode::AbnormalClosure => 1006,
            CloseCode::InvalidFramePayloadData => 1007,
            CloseCode::PolicyViolation => 1008,
            CloseCode::MessageTooBig => 1009,
            CloseCode::MandatoryExt => 1010,
            CloseCode::InternalError => 1011,
            CloseCode::ServiceRestart => 1012,
            CloseCode::TryAgainLater => 1013,
            CloseCode::BadGateway => 1014,
            CloseCode::TlsHandshake => 1015,
            CloseCode::Other(code) => code,
        }
    }
}

impl From<u16> for CloseCode {
    fn from(value: u16) -> Self {
        match value {
            1000 => CloseCode::NormalClosure,
            1001 => CloseCode::GoingAway,
            1002 => CloseCode::ProtocolError,
            1003 => CloseCode::UnsupportedData,
            1005 => CloseCode::NoStatusRcvd,
            1006 => CloseCode::AbnormalClosure,
            1007 => CloseCode::InvalidFramePayloadData,
            1008 => CloseCode::PolicyViolation,
            1009 => CloseCode::MessageTooBig,
            1010 => CloseCode::MandatoryExt,
            1011 => CloseCode::InternalError,
            1012 => CloseCode::ServiceRestart,
            1013 => CloseCode::TryAgainLater,
            1014 => CloseCode::BadGateway,
            1015 => CloseCode::TlsHandshake,
            other => CloseCode::Other(other),
        }
    }
}

/// A future that resolves once a WebSocket has been closed.
pub struct Closed(pub(crate) Pin<Box<dyn Future<Output = io::Result<ClosedReason>>>>);

impl fmt::Debug for Closed {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_tuple("Closed").finish()
    }
}

impl Future for Closed {
    type Output = io::Result<ClosedReason>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context) -> Poll<Self::Output> {
        self.0.poll_unpin(cx)
    }
}