use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use tauri_plugin_opener::OpenerExt;

#[tauri::command]
pub async fn fetch_mod_metadata(game_domain: String, mod_id: u32) -> Result<Value, String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;

    let url = format!("https://api.nexusmods.com/v1/games/{}/mods/{}.json", game_domain, mod_id);
    let mut headers = HeaderMap::new();
    headers.insert("apikey", HeaderValue::from_str(&api_key).unwrap());
    headers.insert("accept", HeaderValue::from_static("application/json"));

    let client = reqwest::Client::new();
    let res = client.get(&url).headers(headers).send().await.map_err(|e| e.to_string())?;

    if res.status().is_success() {
        let json = res.json::<Value>().await.map_err(|e| e.to_string())?;
        Ok(json)
    } else {
        Err(format!("Nexus API Error: {}", res.status()))
    }
}

#[tauri::command]
pub fn open_browser_url(app_handle: tauri::AppHandle, url: String) -> Result<(), String> {
    app_handle.opener().open_url(&url, None::<&str>)
        .map_err(|e| format!("Failed to open browser: {}", e))
}

#[tauri::command]
pub async fn fetch_trending_mods(game_domain: String) -> Result<Value, String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;

    let url = format!("https://api.nexusmods.com/v1/games/{}/mods/trending.json", game_domain);
    let mut headers = HeaderMap::new();
    headers.insert("apikey", HeaderValue::from_str(&api_key).unwrap());
    headers.insert("accept", HeaderValue::from_static("application/json"));

    let client = reqwest::Client::new();
    let res = client.get(&url).headers(headers).send().await.map_err(|e| e.to_string())?;

    if res.status().is_success() {
        let json = res.json::<Value>().await.map_err(|e| e.to_string())?;
        Ok(json)
    } else {
        Err(format!("Nexus API Error: {}", res.status()))
    }
}

#[tauri::command]
pub async fn fetch_latest_mods(game_domain: String) -> Result<Value, String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;
    let url = format!("https://api.nexusmods.com/v1/games/{}/mods/latest_updated.json?period=1m", game_domain);
    let mut headers = HeaderMap::new();
    headers.insert("apikey", HeaderValue::from_str(&api_key).unwrap());
    headers.insert("accept", HeaderValue::from_static("application/json"));

    let client = reqwest::Client::new();
    let res = client.get(&url).headers(headers).send().await.map_err(|e| e.to_string())?;
    
    if res.status().is_success() {
        Ok(res.json::<Value>().await.map_err(|e| e.to_string())?)
    } else {
        Err(format!("Nexus API Error: {}", res.status()))
    }
}

#[tauri::command]
pub async fn search_nexus_mods(offset: u32) -> Result<Value, String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;
    let client = reqwest::Client::new();

    let query = format!(
        r#"{{ mods(filter: {{ gameDomainName: {{ value: "readyornot" }} }}, sort: {{ downloads: {{ direction: DESC }} }}, count: 20, offset: {}) {{ nodes {{ uid modId name summary pictureUrl version author createdAt updatedAt downloads endorsements }} totalCount }} }}"#,
        offset
    );

    println!("[SEARCH] GraphQL query (offset={})", offset);
    let body = serde_json::json!({ "query": query });
    let res = client
        .post("https://api.nexusmods.com/v2/graphql")
        .header("apikey", &api_key)
        .header("Content-Type", "application/json")
        .json(&body)
        .send()
        .await
        .map_err(|e| {
            println!("[SEARCH] Network error: {}", e);
            e.to_string()
        })?;

    println!("[SEARCH] Response status: {}", res.status());

    if res.status().is_success() {
        let json = res.json::<Value>().await.map_err(|e| e.to_string())?;
        println!("[SEARCH] Response keys: {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>()));
        if let Some(errors) = json.get("errors") {
            println!("[SEARCH] GraphQL errors: {}", errors);
        }
        if let Some(data) = json.get("data") {
            if let Some(mods) = data.get("mods") {
                if let Some(nodes) = mods.get("nodes") {
                    println!("[SEARCH] Got {} mod nodes", nodes.as_array().map(|a| a.len()).unwrap_or(0));
                } else {
                    println!("[SEARCH] No 'nodes' field in mods");
                }
            } else {
                println!("[SEARCH] No 'mods' field in data");
            }
        } else {
            println!("[SEARCH] No 'data' field in response. Full response: {}", json);
        }
        Ok(json)
    } else {
        let status = res.status();
        let body_text = res.text().await.unwrap_or_default();
        println!("[SEARCH] Error response: {} - {}", status, body_text);
        Err(format!("Nexus v2 API Error {}: {}", status, body_text))
    }
}

#[tauri::command]
pub async fn download_online_mod(app_handle: tauri::AppHandle, game_domain: String, mod_id: u32) -> Result<(), String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;
    let client = reqwest::Client::new();
    let mut headers = HeaderMap::new();
    headers.insert("apikey", HeaderValue::from_str(&api_key).unwrap());
    headers.insert("accept", HeaderValue::from_static("application/json"));

    let files_url = format!("https://api.nexusmods.com/v1/games/{}/mods/{}/files.json", game_domain, mod_id);
    let files_res = client.get(&files_url).headers(headers.clone()).send().await.map_err(|e| e.to_string())?;
    if !files_res.status().is_success() {
        return Err(format!("Failed to retrieve mod files: {}", files_res.status()));
    }
    let files_json = files_res.json::<Value>().await.map_err(|e| e.to_string())?;
    let files_array = files_json["files"].as_array().ok_or("Invalid files payload")?;
    if files_array.is_empty() {
        return Err("This mod has no files available to download.".to_string());
    }

    let mut target_file = &files_array[0];
    for f in files_array {
        if f["is_primary"].as_bool().unwrap_or(false) {
            target_file = f;
            break;
        }
    }

    let file_id = target_file["file_id"][0].as_u64().unwrap_or_else(|| target_file["file_id"].as_u64().unwrap_or(0));
    if file_id == 0 {
        return Err("Could not determine primary file ID.".to_string());
    }

    let link_url = format!("https://api.nexusmods.com/v1/games/{}/mods/{}/files/{}/download_link.json", game_domain, mod_id, file_id);
    let link_res = client.get(&link_url).headers(headers).send().await.map_err(|e| e.to_string())?;
    if !link_res.status().is_success() {
        return Err(format!("Failed to negotiate download link: {}", link_res.status()));
    }
    let link_json = link_res.json::<Value>().await.map_err(|e| e.to_string())?;
    let links_array = link_json.as_array().ok_or("Missing download links payload")?;
    if links_array.is_empty() { return Err("No download mirrors available.".to_string()); }
    let download_uri = links_array[0]["URI"].as_str().ok_or("URI missing in mirror JSON")?;

    let storage_dir = match crate::config::load_config(app_handle.clone())?.mod_storage_path {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(crate::config::get_default_storage_path(app_handle.clone())),
    };
    let temp_zip = storage_dir.join(format!("downloaded_{}.zip", mod_id));
    let mut download_req = client.get(download_uri).send().await.map_err(|e| e.to_string())?;
    if !download_req.status().is_success() {
        return Err("Failed to stream raw file bytes".to_string());
    }

    let mut file = std::fs::File::create(&temp_zip).map_err(|e| e.to_string())?;
    while let Some(chunk) = download_req.chunk().await.map_err(|e| e.to_string())? {
        use std::io::Write;
        file.write_all(&chunk).map_err(|e| e.to_string())?;
    }

    crate::vfs::install_mod_archive(app_handle.clone(), temp_zip.to_string_lossy().to_string(), mod_id.to_string()).await?;
    std::fs::remove_file(temp_zip).ok();

    Ok(())
}
