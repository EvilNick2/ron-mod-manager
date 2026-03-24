use serde::{Deserialize, Serialize};
use tauri_plugin_opener::OpenerExt;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message, tungstenite::client::IntoClientRequest};
use futures_util::{StreamExt, SinkExt};
use uuid::Uuid;

#[derive(Serialize)]
struct SsoRequest {
    id: String,
    token: Option<String>,
    protocol: u8,
}

#[derive(Deserialize, Debug)]
struct SsoResponse {
    success: bool,
    data: Option<SsoData>,
    error: Option<String>,
}

#[derive(Deserialize, Debug)]
struct SsoData {
    connection_token: Option<String>,
    api_key: Option<String>,
}

#[tauri::command]
pub async fn authenticate_nexus(app_handle: tauri::AppHandle) -> Result<String, String> {
    println!("[SSO] Connecting to Nexus Mods...");
    let url = "wss://sso.nexusmods.com";
    
    let mut request = url.into_client_request()
        .map_err(|e| format!("Bad URL configuration: {}", e))?;
    request.headers_mut().insert(
        "User-Agent",
        "ReadyOrNotModManager/1.0.0".parse().unwrap()
    );
    
    let (ws_stream, response) = connect_async(request)
        .await
        .map_err(|e| format!("Failed to connect to Nexus server: {}", e))?;
    
    println!("[SSO] Connected successfully! HTTP {}", response.status());
    
    let (mut write, mut read) = ws_stream.split();
    
    let uuid_str = Uuid::new_v4().to_string();
    println!("[SSO] Generated local session UUID: {}", uuid_str);
    
    let init_req = SsoRequest {
        id: uuid_str.clone(),
        token: None,
        protocol: 2,
    };
    
    let init_json = serde_json::to_string(&init_req).unwrap();
    write.send(Message::Text(init_json.into()))
        .await
        .map_err(|e| format!("Failed to send SSO initialization request: {}", e))?;
    
    let auth_url = format!("https://www.nexusmods.com/sso?id={}&application=vortex", uuid_str);
    println!("[SSO] Attempting to open browser for authorization: {}", auth_url);
    
    app_handle.opener().open_url(&auth_url, None::<&str>)
        .map_err(|e| format!("Failed to automatically open browser: {}", e))?;
    
    println!("[SSO] Browser opened. Waiting for user to click Authorize on the webpage...");
    
    while let Some(msg) = read.next().await {
        match msg {
            Ok(Message::Text(text)) => {
                println!("[SSO] Received message: {}", text);
                let response: serde_json::Result<SsoResponse> = serde_json::from_str(&text);
                
                if let Ok(resp) = response {
                    if resp.success {
                        if let Some(data) = resp.data {
                            if let Some(api_key) = data.api_key {
                                println!("[SSO] Authentication successful! Key received.");
                                
                                if let Err(e) = crate::config::save_api_key(api_key.clone()) {
                                    println!("[SSO] WARNING: Failed to save API key persistently: {}", e);
                                }
                                
                                return Ok(api_key);
                            } else if let Some(token) = data.connection_token {
                                println!("[SSO] Received connection token: {}", token);
                            }
                        }
                    } else if let Some(err) = resp.error {
                        println!("[SSO] Authentication failed: {}", err);
                        return Err(err);
                    }
                }
            }
            Ok(Message::Ping(_)) => continue,
            _ => {}
        }
    }
    
    Err("Connection closed before authorization completed.".into())
}
