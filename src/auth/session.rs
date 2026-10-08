//! Mojang session server: proving account ownership to a third party.
//!
//! A server (a Minecraft server, or any backend such as a blueprint site) hands
//! out a `server_id`. The client calls [`join_server`] with its Minecraft access
//! token, then the server asks Mojang's `hasJoined` endpoint whether that player
//! joined with that `server_id`. The access token itself never leaves the client.

use serde::Deserialize;

use crate::error::{HexoError, Result};

const JOIN_URL: &str = "https://sessionserver.mojang.com/session/minecraft/join";

#[derive(Deserialize)]
struct SessionError {
    #[serde(default)]
    error: String,
    #[serde(default, rename = "errorMessage")]
    error_message: String,
}

/// Tell Mojang this player is joining the server identified by `server_id`.
///
/// `access_token` and `uuid` come from [`AuthResult`](super::AuthResult); the
/// UUID may be given with or without dashes.
pub async fn join_server(access_token: &str, uuid: &str, server_id: &str) -> Result<()> {
    let resp = reqwest::Client::new()
        .post(JOIN_URL)
        .json(&serde_json::json!({
            "accessToken": access_token,
            "selectedProfile": uuid.replace('-', ""),
            "serverId": server_id,
        }))
        .send()
        .await?;

    let status = resp.status();
    if status.is_success() {
        return Ok(());
    }

    let body = resp.text().await.unwrap_or_default();
    let detail = match serde_json::from_str::<SessionError>(&body) {
        Ok(e) if !e.error_message.is_empty() => format!("{}: {}", e.error, e.error_message),
        _ => format!("HTTP {status}"),
    };
    Err(HexoError::AuthError(format!("session join failed ({detail})")))
}
