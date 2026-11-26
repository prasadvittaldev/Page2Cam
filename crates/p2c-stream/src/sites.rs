//! Per-site stream state, sender backoff, exit classification and the
//! combined state. Pure logic: no processes.
use std::fmt;
use std::time::Duration;

use crate::{Site, StreamState};

/// State of one destination's sender.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SiteState {
    Idle,
    Connecting,
    Live,
    Reconnecting { attempt: u32 },
    Error(String),
}

impl fmt::Display for SiteState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SiteState::Idle => write!(f, "idle"),
            SiteState::Connecting => write!(f, "connecting"),
            SiteState::Live => write!(f, "live"),
            SiteState::Reconnecting { attempt } => write!(f, "reconnecting ({attempt})"),
            SiteState::Error(e) => write!(f, "error: {e}"),
        }
    }
}

/// Wait before sender restart `attempt` (1-based): 2, 4, 8, 16, then 30 s.
pub fn retry_delay(attempt: u32) -> Duration {
    match attempt {
        0 | 1 => Duration::from_secs(2),
        2 => Duration::from_secs(4),
        3 => Duration::from_secs(8),
        4 => Duration::from_secs(16),
        _ => Duration::from_secs(30),
    }
}

/// Why a sender process ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SenderExit {
    /// The site refused the key or login: retrying won't help.
    Auth(String),
    /// Anything else (network, server): worth retrying.
    Other(String),
}

const AUTH_WORDS: [&str; 7] = [
    "401",
    "403",
    "unauthorized",
    "forbidden",
    "invalid stream key",
    "authentication",
    "netstream.publish.badname",
];

/// Classify a sender's last output lines. The reason is the last non-empty line.
pub fn classify_exit(lines: &[String]) -> SenderExit {
    let reason = lines
        .iter()
        .rev()
        .map(|l| l.trim())
        .find(|l| !l.is_empty())
        .unwrap_or("sender exited")
        .to_string();
    let auth = lines.iter().any(|l| {
        let l = l.to_lowercase();
        AUTH_WORDS.iter().any(|w| l.contains(w))
    });
    if auth {
        SenderExit::Auth(reason)
    } else {
        SenderExit::Other(reason)
    }
}

/// Combine the encoder and per-site states into the one shown to the user.
/// The bool says the whole stream must stop.
pub fn overall(sites: &[(Site, SiteState)], encoder_error: Option<&str>) -> (StreamState, bool) {
    if let Some(e) = encoder_error {
        return (StreamState::Error(e.to_string()), true);
    }
    if sites.iter().any(|(_, s)| *s == SiteState::Live) {
        return (StreamState::Live, false);
    }
    if !sites.is_empty() && sites.iter().all(|(_, s)| matches!(s, SiteState::Error(_))) {
        let msg = sites
            .iter()
            .filter_map(|(site, s)| match s {
                SiteState::Error(e) => Some(format!("{}: {e}", site.label())),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("; ");
        return (StreamState::Error(msg), true);
    }
    (StreamState::Connecting, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Site, StreamState};

    #[test]
    fn backoff_doubles_to_thirty() {
        let s: Vec<u64> = (1..=7).map(|a| retry_delay(a).as_secs()).collect();
        assert_eq!(s, [2, 4, 8, 16, 30, 30, 30]);
    }
    #[test]
    fn bad_key_is_auth() {
        let l = vec!["[rtmp @ 0x1] Server error: NetStream.Publish.BadName".to_string()];
        assert!(matches!(classify_exit(&l), SenderExit::Auth(_)));
        let n = vec!["Connection to tcp://x:1935 failed: Connection refused".to_string()];
        assert!(matches!(classify_exit(&n), SenderExit::Other(_)));
    }
    #[test]
    fn overall_state_rules() {
        use SiteState::*;
        let yt = |s| (Site::YouTube, s);
        let tw = |s| (Site::Twitch, s);
        assert_eq!(
            overall(&[yt(Live), tw(Reconnecting { attempt: 2 })], None),
            (StreamState::Live, false)
        );
        assert_eq!(
            overall(&[yt(Connecting), tw(Error("x".into()))], None),
            (StreamState::Connecting, false)
        );
        assert_eq!(
            overall(
                &[yt(Error("bad key".into())), tw(Error("refused".into()))],
                None
            ),
            (
                StreamState::Error("YouTube: bad key; Twitch: refused".into()),
                true
            )
        );
        assert_eq!(
            overall(&[yt(Live)], Some("encoder died")),
            (StreamState::Error("encoder died".into()), true)
        );
    }
    #[test]
    fn display_and_auth_words() {
        assert_eq!(
            SiteState::Reconnecting { attempt: 3 }.to_string(),
            "reconnecting (3)"
        );
        assert_eq!(SiteState::Error("x".into()).to_string(), "error: x");
        let l = vec!["HTTP error 403 Forbidden".to_string(), "".to_string()];
        assert_eq!(
            classify_exit(&l),
            SenderExit::Auth("HTTP error 403 Forbidden".into())
        );
    }
}
