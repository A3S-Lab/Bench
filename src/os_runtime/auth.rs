use anyhow::{Context, Result};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

const REFRESH_SKEW_MS: u64 = 120_000;
static SESSION_CACHE: OnceLock<Mutex<Option<AuthSession>>> = OnceLock::new();

#[derive(Debug, Deserialize)]
struct AuthState {
    sessions: Vec<AuthSession>,
}

#[derive(Debug, Clone, Deserialize)]
struct AuthSession {
    address: String,
    access_token: String,
    refresh_token: Option<String>,
    expires_at_ms: Option<u64>,
}

pub(super) fn credentials(http: &Client) -> Result<(String, String)> {
    if let Some((address, access_token)) = environment_credentials()? {
        return Ok((normalize_address(&address)?, access_token));
    }
    let cache = SESSION_CACHE.get_or_init(|| Mutex::new(None));
    let mut cached = cache
        .lock()
        .map_err(|_| anyhow::anyhow!("A3S OS auth cache is poisoned"))?;
    let mut session = if let Some(session) = cached.as_ref() {
        session.clone()
    } else {
        load_auth_session()?
    };
    let address = normalize_address(&session.address)?;
    let now_ms = unix_time_ms()?;
    if needs_refresh(&session, now_ms) {
        let refresh_token = session
            .refresh_token
            .as_deref()
            .expect("refresh decision requires a refresh token");
        let response = http
            .post(format!("{address}/api/v1/auth/refresh"))
            .json(&json!({"refreshToken": refresh_token}))
            .send()
            .context("could not refresh A3S OS login")?;
        let value = super::response_json(response)?;
        session.access_token = super::data_field(&value, "accessToken")?
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("A3S OS refresh returned an invalid accessToken"))?
            .to_owned();
        session.expires_at_ms = super::data_field(&value, "expiresIn")
            .ok()
            .and_then(Value::as_u64)
            .and_then(|seconds| seconds.checked_mul(1_000))
            .and_then(|duration| now_ms.checked_add(duration));
    }
    anyhow::ensure!(
        !session.access_token.is_empty(),
        "A3S OS access token is empty"
    );
    *cached = Some(session.clone());
    Ok((address, session.access_token))
}

fn environment_credentials() -> Result<Option<(String, String)>> {
    let address = environment_value("A3S_OS_ADDRESS")?;
    let access_token = environment_value("A3S_OS_ACCESS_TOKEN")?;
    validate_environment_credentials(address, access_token)
}

fn environment_value(name: &str) -> Result<Option<String>> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| anyhow::anyhow!("{name} is not valid UTF-8"))
        })
        .transpose()
}

fn validate_environment_credentials(
    address: Option<String>,
    access_token: Option<String>,
) -> Result<Option<(String, String)>> {
    match (address, access_token) {
        (None, None) => Ok(None),
        (Some(address), Some(access_token)) => {
            anyhow::ensure!(
                !address.trim().is_empty() && !access_token.trim().is_empty(),
                "A3S_OS_ADDRESS and A3S_OS_ACCESS_TOKEN must both be non-empty"
            );
            Ok(Some((address, access_token)))
        }
        _ => anyhow::bail!("A3S_OS_ADDRESS and A3S_OS_ACCESS_TOKEN must be configured together"),
    }
}

fn load_auth_session() -> Result<AuthSession> {
    let path = os_auth_path()?;
    let state: AuthState = serde_json::from_slice(
        &std::fs::read(&path)
            .with_context(|| format!("could not read A3S OS auth at {}", path.display()))?,
    )?;
    state
        .sessions
        .into_iter()
        .find(|session| !session.address.trim().is_empty())
        .ok_or_else(|| anyhow::anyhow!("A3S OS auth has no session"))
}

fn needs_refresh(session: &AuthSession, now_ms: u64) -> bool {
    session.refresh_token.is_some()
        && session
            .expires_at_ms
            .is_none_or(|expires_at| now_ms.saturating_add(REFRESH_SKEW_MS) >= expires_at)
}

fn unix_time_ms() -> Result<u64> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis();
    u64::try_from(millis).context("system clock exceeds the supported millisecond range")
}

fn normalize_address(value: &str) -> Result<String> {
    let value = value.trim().trim_end_matches('/');
    anyhow::ensure!(
        value.starts_with("http://") || value.starts_with("https://"),
        "A3S OS address must use http:// or https://"
    );
    Ok(value.to_owned())
}

fn os_auth_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| {
        anyhow::anyhow!("HOME is unavailable; set A3S_OS_ADDRESS and A3S_OS_ACCESS_TOKEN")
    })?;
    Ok(home.join(".a3s/os-auth.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshes_only_when_expiry_is_unknown_or_near() {
        let mut session = AuthSession {
            address: "https://os.example.test".into(),
            access_token: "access".into(),
            refresh_token: Some("refresh".into()),
            expires_at_ms: Some(1_000_000),
        };
        assert!(!needs_refresh(&session, 100_000));
        assert!(needs_refresh(&session, 900_000));
        session.expires_at_ms = None;
        assert!(needs_refresh(&session, 100_000));
        session.refresh_token = None;
        assert!(!needs_refresh(&session, 900_000));
    }

    #[test]
    fn rejects_partial_environment_credentials() {
        assert!(validate_environment_credentials(None, None)
            .unwrap()
            .is_none());
        assert!(
            validate_environment_credentials(Some("https://os.example.test".into()), None).is_err()
        );
        assert!(validate_environment_credentials(None, Some("token".into())).is_err());
        assert!(
            validate_environment_credentials(Some(String::new()), Some("token".into())).is_err()
        );
    }

    #[test]
    fn normalizes_server_address() {
        assert_eq!(
            normalize_address(" https://os.example.test/ ").unwrap(),
            "https://os.example.test"
        );
        assert!(normalize_address("os.example.test").is_err());
    }
}
