use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};

use crate::error::{HexoError, Result};

const MS_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const MS_TOKEN_URL: &str = "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str = "https://api.minecraftservices.com/authentication/login_with_xbox";
const MC_PROFILE_URL: &str = "https://api.minecraftservices.com/minecraft/profile";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResult {
    pub player_name: String,
    pub uuid: String,
    pub access_token: String,
    pub xuid: String,
    pub refresh_token: String,
    /// Token expiry as a Unix timestamp (seconds).
    pub expires_at: u64,
}

/// Device code details shown to the user.
#[derive(Debug, Clone)]
pub struct DeviceCodeInfo {
    /// URL for the user to open.
    pub verification_uri: String,
    /// Code for the user to enter.
    pub user_code: String,
    /// Ready-to-display message.
    pub message: String,
}

/// Microsoft authentication client configured with the caller's Entra application ID.
///
/// The application must support personal Microsoft accounts and public-client device
/// code flow. Minecraft/Xbox services may additionally require the application to be
/// approved by Microsoft. A library must not reuse the official launcher's client ID.
#[derive(Debug, Clone)]
pub struct MicrosoftAuth {
    client_id: String,
    client: reqwest::Client,
}

impl MicrosoftAuth {
    pub fn new(client_id: impl Into<String>) -> Result<Self> {
        let client_id = client_id.into();
        if client_id.trim().is_empty() {
            return Err(HexoError::AuthError("Microsoft client ID is empty".into()));
        }
        Ok(Self {
            client_id,
            client: reqwest::Client::new(),
        })
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub async fn request_device_code(&self) -> Result<(DeviceCodeInfo, MsDeviceCodeResponse)> {
        let response = self
            .client
            .post(MS_DEVICE_CODE_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("scope", "XboxLive.signin offline_access"),
            ])
            .send()
            .await?;
        let mut resp: MsDeviceCodeResponse = auth_json(response, "Microsoft device code").await?;
        resp.client_id = self.client_id.clone();

        let info = DeviceCodeInfo {
            verification_uri: resp.verification_uri.clone(),
            user_code: resp.user_code.clone(),
            message: resp.message.clone(),
        };
        Ok((info, resp))
    }

    pub async fn poll_device_code(&self, device_resp: &MsDeviceCodeResponse) -> Result<AuthResult> {
        if !device_resp.client_id.is_empty() && device_resp.client_id != self.client_id {
            return Err(HexoError::AuthError(
                "device code was issued for a different Microsoft client ID".into(),
            ));
        }

        let mut interval = Duration::from_secs(u64::from(device_resp.interval.max(1)));
        let deadline = unix_time()?.saturating_add(u64::from(device_resp.expires_in));

        loop {
            sleep(interval).await;
            if unix_time()? >= deadline {
                return Err(HexoError::AuthError("device code expired".to_string()));
            }

            let response = self
                .client
                .post(MS_TOKEN_URL)
                .form(&[
                    ("client_id", self.client_id.as_str()),
                    ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                    ("device_code", device_resp.device_code.as_str()),
                ])
                .send()
                .await?;
            let status = response.status();
            let token_resp: serde_json::Value = response.json().await?;

            if let Some(err) = token_resp.get("error").and_then(|v| v.as_str()) {
                match err {
                    "authorization_pending" => continue,
                    "slow_down" => {
                        interval += Duration::from_secs(5);
                        continue;
                    }
                    _ => return Err(auth_payload_error("Microsoft token", status, &token_resp)),
                }
            }
            if !status.is_success() {
                return Err(auth_payload_error("Microsoft token", status, &token_resp));
            }

            let ms_access_token = required_string(&token_resp, "access_token", "Microsoft token")?;
            let refresh_token = token_resp["refresh_token"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let expires_at =
                unix_time()?.saturating_add(token_resp["expires_in"].as_u64().unwrap_or(3600));
            return complete_auth(&self.client, &ms_access_token, refresh_token, expires_at).await;
        }
    }

    pub async fn refresh_token(&self, refresh_token: &str) -> Result<AuthResult> {
        let response = self
            .client
            .post(MS_TOKEN_URL)
            .form(&[
                ("client_id", self.client_id.as_str()),
                ("grant_type", "refresh_token"),
                ("refresh_token", refresh_token),
                ("scope", "XboxLive.signin offline_access"),
            ])
            .send()
            .await?;
        let token_resp: serde_json::Value = auth_json(response, "Microsoft token refresh").await?;
        let ms_access_token =
            required_string(&token_resp, "access_token", "Microsoft token refresh")?;
        let new_refresh = token_resp["refresh_token"]
            .as_str()
            .unwrap_or(refresh_token)
            .to_string();
        let expires_at =
            unix_time()?.saturating_add(token_resp["expires_in"].as_u64().unwrap_or(3600));
        complete_auth(&self.client, &ms_access_token, new_refresh, expires_at).await
    }
}

async fn complete_auth(
    client: &reqwest::Client,
    ms_access_token: &str,
    refresh_token: String,
    expires_at: u64,
) -> Result<AuthResult> {
    // XBL
    let xbl_resp: serde_json::Value = auth_json(
        client
            .post(XBL_URL)
            .json(&serde_json::json!({
                "Properties": {
                    "AuthMethod": "RPS",
                    "SiteName": "user.auth.xboxlive.com",
                    "RpsTicket": format!("d={}", ms_access_token)
                },
                "RelyingParty": "http://auth.xboxlive.com",
                "TokenType": "JWT"
            }))
            .send()
            .await?,
        "Xbox Live authentication",
    )
    .await?;

    let xbl_token = xbl_resp["Token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("Xbox Live token request failed".into()))?
        .to_string();

    let user_hash = xbl_resp["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("Xbox Live uhs missing".into()))?
        .to_string();

    // XSTS
    let xsts_resp: serde_json::Value = auth_json(
        client
            .post(XSTS_URL)
            .json(&serde_json::json!({
                "Properties": {
                    "SandboxId": "RETAIL",
                    "UserTokens": [xbl_token]
                },
                "RelyingParty": "rp://api.minecraftservices.com/",
                "TokenType": "JWT"
            }))
            .send()
            .await?,
        "XSTS authorization",
    )
    .await?;

    if let Some(xerr) = xsts_resp.get("XErr") {
        return Err(HexoError::AuthError(format!("XSTS error: {}", xerr)));
    }

    let xsts_token = xsts_resp["Token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("XSTS token request failed".into()))?
        .to_string();

    let xuid = xsts_resp["DisplayClaims"]["xui"][0]["xid"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // MC Token
    let mc_resp: serde_json::Value = auth_json(
        client
            .post(MC_LOGIN_URL)
            .json(&serde_json::json!({
                "identityToken": format!("XBL3.0 x={};{}", user_hash, xsts_token)
            }))
            .send()
            .await?,
        "Minecraft authentication",
    )
    .await?;

    let mc_token = mc_resp["access_token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("Minecraft token request failed".into()))?
        .to_string();

    // MC Profile
    let profile_resp: serde_json::Value = auth_json(
        client
            .get(MC_PROFILE_URL)
            .bearer_auth(&mc_token)
            .send()
            .await?,
        "Minecraft profile",
    )
    .await?;

    if profile_resp.get("error").is_some() {
        return Err(HexoError::AuthError(
            "account does not own Minecraft".to_string(),
        ));
    }

    let player_name = profile_resp["name"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("could not read the player name".into()))?
        .to_string();

    let uuid = profile_resp["id"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("could not read the UUID".into()))?
        .to_string();

    Ok(AuthResult {
        player_name,
        uuid,
        access_token: mc_token,
        xuid,
        refresh_token,
        expires_at,
    })
}

#[derive(Debug, Deserialize)]
pub struct MsDeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u32,
    pub interval: u32,
    pub message: String,
    #[serde(skip)]
    client_id: String,
}

fn unix_time() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| HexoError::AuthError(format!("system clock error: {error}")))
}

fn required_string(value: &serde_json::Value, key: &str, service: &str) -> Result<String> {
    value[key]
        .as_str()
        .map(ToOwned::to_owned)
        .ok_or_else(|| HexoError::AuthError(format!("{service} response is missing {key}")))
}

async fn auth_json<T: DeserializeOwned>(response: reqwest::Response, service: &str) -> Result<T> {
    let status = response.status();
    let payload: serde_json::Value = response.json().await?;
    if !status.is_success() || payload.get("error").is_some() || payload.get("XErr").is_some() {
        return Err(auth_payload_error(service, status, &payload));
    }
    serde_json::from_value(payload).map_err(Into::into)
}

fn auth_payload_error(
    service: &str,
    status: reqwest::StatusCode,
    payload: &serde_json::Value,
) -> HexoError {
    let detail = payload
        .get("error_description")
        .or_else(|| payload.get("errorMessage"))
        .or_else(|| payload.get("message"))
        .or_else(|| payload.get("error"))
        .or_else(|| payload.get("XErr"))
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| value.to_string())
        })
        .unwrap_or_else(|| "unknown error".into());
    HexoError::AuthError(format!("{service} failed ({status}): {detail}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_errors_include_service_detail() {
        let payload = serde_json::json!({
            "error": "invalid_client",
            "error_description": "The client application is not configured correctly"
        });
        let error = auth_payload_error(
            "Microsoft device code",
            reqwest::StatusCode::BAD_REQUEST,
            &payload,
        );
        let message = error.to_string();
        assert!(message.contains("Microsoft device code"));
        assert!(message.contains("not configured correctly"));
    }

    #[test]
    fn empty_client_id_is_rejected() {
        assert!(matches!(
            MicrosoftAuth::new("  "),
            Err(HexoError::AuthError(_))
        ));
    }
}
