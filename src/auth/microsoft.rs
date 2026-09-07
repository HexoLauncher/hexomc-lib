use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::{sleep, Duration};

use crate::error::{HexoError, Result};

// Microsoft Azure app (Minecraft Launcher's public client_id).
const MS_CLIENT_ID: &str = "00000000402b5328";
const MS_DEVICE_CODE_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/devicecode";
const MS_TOKEN_URL: &str =
    "https://login.microsoftonline.com/consumers/oauth2/v2.0/token";
const XBL_URL: &str = "https://user.auth.xboxlive.com/user/authenticate";
const XSTS_URL: &str = "https://xsts.auth.xboxlive.com/xsts/authorize";
const MC_LOGIN_URL: &str =
    "https://api.minecraftservices.com/authentication/login_with_xbox";
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

/// Step 1: request a device code (return `DeviceCodeInfo` to show the user),
/// then call `poll_device_code()`.
pub async fn request_device_code() -> Result<(DeviceCodeInfo, MsDeviceCodeResponse)> {
    let client = reqwest::Client::new();
    let resp: MsDeviceCodeResponse = client
        .post(MS_DEVICE_CODE_URL)
        .form(&[
            ("client_id", MS_CLIENT_ID),
            ("scope", "XboxLive.signin offline_access"),
        ])
        .send()
        .await?
        .json()
        .await?;

    let info = DeviceCodeInfo {
        verification_uri: resp.verification_uri.clone(),
        user_code: resp.user_code.clone(),
        message: resp.message.clone(),
    };

    Ok((info, resp))
}

/// Poll for the MS token, then run the full OAuth chain to an `AuthResult`.
pub async fn poll_device_code(device_resp: &MsDeviceCodeResponse) -> Result<AuthResult> {
    let client = reqwest::Client::new();
    let interval = Duration::from_secs(device_resp.interval as u64);
    let deadline =
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + device_resp.expires_in as u64;

    loop {
        sleep(interval).await;

        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        if now > deadline {
            return Err(HexoError::AuthError("裝置代碼已過期".to_string()));
        }

        let token_resp: serde_json::Value = client
            .post(MS_TOKEN_URL)
            .form(&[
                ("client_id", MS_CLIENT_ID),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", &device_resp.device_code),
            ])
            .send()
            .await?
            .json()
            .await?;

        if let Some(err) = token_resp.get("error") {
            match err.as_str().unwrap_or("") {
                "authorization_pending" => continue,
                "slow_down" => {
                    sleep(Duration::from_secs(5)).await;
                    continue;
                }
                other => {
                    return Err(HexoError::AuthError(format!("MS token 錯誤: {}", other)));
                }
            }
        }

        let ms_access_token = token_resp["access_token"]
            .as_str()
            .ok_or_else(|| HexoError::AuthError("缺少 MS access_token".into()))?
            .to_string();

        let refresh_token = token_resp["refresh_token"]
            .as_str()
            .unwrap_or("")
            .to_string();

        let expires_in: u64 = token_resp["expires_in"].as_u64().unwrap_or(3600);
        let expires_at =
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + expires_in;

        return complete_auth(&client, &ms_access_token, refresh_token, expires_at).await;
    }
}

/// Refresh the access token using a refresh token.
pub async fn refresh_token(refresh_token: &str) -> Result<AuthResult> {
    let client = reqwest::Client::new();
    let token_resp: serde_json::Value = client
        .post(MS_TOKEN_URL)
        .form(&[
            ("client_id", MS_CLIENT_ID),
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ])
        .send()
        .await?
        .json()
        .await?;

    if let Some(err) = token_resp.get("error") {
        return Err(HexoError::AuthError(format!(
            "refresh token 失敗: {}",
            err.as_str().unwrap_or("unknown")
        )));
    }

    let ms_access_token = token_resp["access_token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("缺少 access_token".into()))?
        .to_string();

    let new_refresh = token_resp["refresh_token"]
        .as_str()
        .unwrap_or(refresh_token)
        .to_string();

    let expires_in: u64 = token_resp["expires_in"].as_u64().unwrap_or(3600);
    let expires_at =
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() + expires_in;

    complete_auth(&client, &ms_access_token, new_refresh, expires_at).await
}

async fn complete_auth(
    client: &reqwest::Client,
    ms_access_token: &str,
    refresh_token: String,
    expires_at: u64,
) -> Result<AuthResult> {
    // XBL
    let xbl_resp: serde_json::Value = client
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
        .await?
        .json()
        .await?;

    let xbl_token = xbl_resp["Token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("XBL token 失敗".into()))?
        .to_string();

    let user_hash = xbl_resp["DisplayClaims"]["xui"][0]["uhs"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("XBL uhs 失敗".into()))?
        .to_string();

    // XSTS
    let xsts_resp: serde_json::Value = client
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
        .await?
        .json()
        .await?;

    if let Some(xerr) = xsts_resp.get("XErr") {
        return Err(HexoError::AuthError(format!("XSTS 錯誤: {}", xerr)));
    }

    let xsts_token = xsts_resp["Token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("XSTS token 失敗".into()))?
        .to_string();

    let xuid = xsts_resp["DisplayClaims"]["xui"][0]["xid"]
        .as_str()
        .unwrap_or("")
        .to_string();

    // MC Token
    let mc_resp: serde_json::Value = client
        .post(MC_LOGIN_URL)
        .json(&serde_json::json!({
            "identityToken": format!("XBL3.0 x={};{}", user_hash, xsts_token)
        }))
        .send()
        .await?
        .json()
        .await?;

    let mc_token = mc_resp["access_token"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("MC token 失敗".into()))?
        .to_string();

    // MC Profile
    let profile_resp: serde_json::Value = client
        .get(MC_PROFILE_URL)
        .bearer_auth(&mc_token)
        .send()
        .await?
        .json()
        .await?;

    if profile_resp.get("error").is_some() {
        return Err(HexoError::AuthError("帳號未購買 Minecraft".to_string()));
    }

    let player_name = profile_resp["name"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("無法取得玩家名稱".into()))?
        .to_string();

    let uuid = profile_resp["id"]
        .as_str()
        .ok_or_else(|| HexoError::AuthError("無法取得 UUID".into()))?
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
}
