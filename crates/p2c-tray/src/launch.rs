//! Building the engine's command line. The token goes through the
//! environment only (never visible in a process listing).

use std::net::TcpListener;

pub const TOKEN_ENV: &str = "P2C_ADMIN_TOKEN";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EngineCommand {
    /// Arguments after the exe path.
    pub args: Vec<String>,
    /// Extra environment variables for the child.
    pub env: Vec<(String, String)>,
}

/// Same rule the engine applies: 16..=128 chars of `[A-Za-z0-9_-]`.
pub fn valid_token(t: &str) -> bool {
    (16..=128).contains(&t.len())
        && t.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// `None` if the token would be rejected by the engine.
pub fn engine_command(port: u16, tray_pid: u32, token: &str) -> Option<EngineCommand> {
    if !valid_token(token) {
        return None;
    }
    Some(EngineCommand {
        args: vec![
            "--engine".into(),
            "--camera-from-tray".into(),
            "--admin-port".into(),
            port.to_string(),
            "--parent-pid".into(),
            tray_pid.to_string(),
        ],
        env: vec![(TOKEN_ENV.into(), token.into())],
    })
}

/// A currently free loopback port (the engine binds it strictly; a lost race
/// shows up as exit 76 and one retry).
pub fn pick_free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind(("127.0.0.1", 0))?.local_addr()?.port())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_never_in_args() {
        let token = "abcDEF0123456789_-xyzXYZ";
        let c = engine_command(8765, 4242, token).unwrap();
        assert!(c.args.iter().all(|a| !a.contains(token)), "{:?}", c.args);
        assert!(c
            .env
            .contains(&("P2C_ADMIN_TOKEN".to_string(), token.to_string())));
        for flag in [
            "--engine",
            "--camera-from-tray",
            "--admin-port",
            "8765",
            "--parent-pid",
            "4242",
        ] {
            assert!(c.args.iter().any(|a| a == flag), "{flag}");
        }
        let pos = |f: &str| c.args.iter().position(|a| a == f).unwrap();
        assert_eq!(c.args[pos("--admin-port") + 1], "8765");
        assert_eq!(c.args[pos("--parent-pid") + 1], "4242");
    }

    #[test]
    fn invalid_tokens_are_refused() {
        assert!(engine_command(1, 1, "short").is_none());
        assert!(engine_command(1, 1, &"a".repeat(129)).is_none());
        assert!(engine_command(1, 1, "has space has space 123").is_none());
        assert!(valid_token(&"a".repeat(16)));
    }

    #[test]
    fn free_port_is_usable() {
        let p = pick_free_port().unwrap();
        assert!(p > 0);
        std::net::TcpListener::bind(("127.0.0.1", p)).unwrap();
    }
}
