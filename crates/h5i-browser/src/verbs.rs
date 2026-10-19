//! The control channel's verbs, and the failures a verb can answer with.
use serde_json::{json, Value};

/// Everything the control channel can be asked to do. Page verbs are not here:
/// the CLI sends those to the session's Chromium through agent-browser.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    /// What the session is, for a client that just connected.
    Status,
    /// The request log, as the proxy wrote it.
    Requests,
    /// Send a stored request again, with changes.
    Resend,
    /// Which credentials this session could substitute, by name.
    Env,
    /// Open a WebSocket, send frames, and report what came back.
    ///
    /// `resend` is the same idea for HTTP: an application whose commands
    /// travel over a socket is one this workbench can speak to, not only watch.
    Socket,
    /// Make one gRPC call over HTTP/2 and report its frames and status.
    ///
    /// The plugin frames the bytes; this puts them on the wire under the
    /// session's policy and records the exchange.
    Grpc,
}

impl Verb {
    /// Every verb. A variant missing here compiles and does not exist on the
    /// wire, so add new verbs here first.
    pub const ALL: &'static [Verb] = &[
        Verb::Status,
        Verb::Requests,
        Verb::Resend,
        Verb::Env,
        Verb::Socket,
        Verb::Grpc,
    ];

    /// The name on the wire.
    pub fn name(self) -> &'static str {
        match self {
            Verb::Status => "status",
            Verb::Requests => "requests",
            Verb::Resend => "resend",
            Verb::Env => "env",
            Verb::Socket => "socket",
            Verb::Grpc => "grpc",
        }
    }

    /// Resolve a wire name.
    pub fn from_name(name: &str) -> Option<Verb> {
        Verb::ALL.iter().copied().find(|v| v.name() == name)
    }

    /// The verb list, for a caller that named something else.
    pub fn known() -> String {
        Verb::ALL
            .iter()
            .map(|v| v.name())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Why a verb could not be done, in a form an agent can act on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerbError {
    pub code: Code,
    pub message: String,
}

/// The machine-readable half of a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Code {
    /// The verb name is not one this session has.
    UnknownVerb,
    /// A required argument is missing, or is the wrong shape.
    BadRequest,
    /// The policy refused it.
    Refused,
    /// The session failed at something that is not the caller's fault.
    Internal,
}

impl Code {
    pub fn as_str(self) -> &'static str {
        match self {
            Code::UnknownVerb => "unknown-verb",
            Code::BadRequest => "bad-request",
            Code::Refused => "refused",
            Code::Internal => "internal",
        }
    }

    /// Whether this is the caller's mistake to fix, so a retry can help.
    pub fn caller_can_fix(self) -> bool {
        match self {
            Code::BadRequest | Code::UnknownVerb => true,
            Code::Refused | Code::Internal => false,
        }
    }
}

impl VerbError {
    pub fn new(code: Code, message: impl Into<String>) -> Self {
        VerbError {
            code,
            message: message.into(),
        }
    }

    /// The reply an agent receives.
    pub fn reply(&self) -> Value {
        json!({
            "ok": false,
            "code": self.code.as_str(),
            "error": self.message,
            "retryable": self.code.caller_can_fix(),
        })
    }

    pub fn unknown_verb(name: &str) -> Self {
        VerbError::new(
            Code::UnknownVerb,
            format!(
                "`{name}` is not a verb this session has. It knows: {}. The page itself is \
                 the Chromium this session's proxy captures; `h5i browser snapshot`, `eval` and \
                 the other page verbs reach it through agent-browser.",
                Verb::known()
            ),
        )
    }

    pub fn bad_request(message: impl Into<String>) -> Self {
        VerbError::new(Code::BadRequest, message)
    }

    pub fn refused(message: impl Into<String>) -> Self {
        VerbError::new(Code::Refused, message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every verb this session implements is a verb it will answer to.
    #[test]
    fn every_name_round_trips_and_none_collide() {
        let mut seen = std::collections::HashSet::new();
        for verb in Verb::ALL {
            assert!(seen.insert(verb.name()), "two verbs answer to {:?}", verb.name());
            assert_eq!(Verb::from_name(verb.name()), Some(*verb));
            assert!(Verb::known().contains(verb.name()));
        }
        assert_eq!(Verb::from_name("resend"), Some(Verb::Resend));
        assert_eq!(Verb::from_name("socket"), Some(Verb::Socket));
        assert_eq!(Verb::from_name("snapshot"), None);
    }

    #[test]
    fn a_policy_refusal_is_not_offered_as_retryable() {
        assert!(!Code::Refused.caller_can_fix());
        assert!(Code::BadRequest.caller_can_fix());
        let reply = VerbError::refused("no").reply();
        assert_eq!(reply["code"], "refused");
        assert_eq!(reply["retryable"], false);
    }

    #[test]
    fn the_unknown_verb_message_lists_what_is_known() {
        let text = VerbError::unknown_verb("typewrite").message;
        assert!(text.contains("typewrite"));
        for verb in Verb::ALL {
            assert!(text.contains(verb.name()), "{} missing", verb.name());
        }
    }
}
