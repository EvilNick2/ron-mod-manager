use reqwest::header::{HeaderMap, HeaderValue};
use serde_json::Value;
use tauri_plugin_opener::OpenerExt;

async fn execute_graphql_query(client: &reqwest::Client, api_key: &str, query: String) -> Result<Value, String> {
    let body = serde_json::json!({ "query": query });
    let res = client
        .post("https://api.nexusmods.com/v2/graphql")
        .header("apikey", api_key)
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
        res.json::<Value>().await.map_err(|e| e.to_string())
    } else {
        let status = res.status();
        let body_text = res.text().await.unwrap_or_default();
        println!("[SEARCH] Error response: {} - {}", status, body_text);
        Err(format!("Nexus v2 API Error {}: {}", status, body_text))
    }
}

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
pub fn open_game_path(app_handle: tauri::AppHandle) -> Result<(), String> {
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    if let Some(path) = config.game_path {
        let mut paks_mod_dir = std::path::PathBuf::from(path);
        if !paks_mod_dir.ends_with("Paks") {
            paks_mod_dir.push("ReadyOrNot");
            paks_mod_dir.push("Content");
            paks_mod_dir.push("Paks");
        }
        
        app_handle.opener().open_path(paks_mod_dir.to_string_lossy().to_string(), None::<&str>)
            .map_err(|e| format!("Failed to open folder: {}", e))
    } else {
        Err("Game path is not set yet.".to_string())
    }
}

#[tauri::command]
pub fn launch_game(app_handle: tauri::AppHandle) -> Result<(), String> {
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    let app_id = config.game_app_id.unwrap_or(1144200);
    
    let launch_url = if let Some(opts) = config.launch_options {
        if opts.trim().is_empty() {
            format!("steam://run/{}", app_id)
        } else {
            format!("steam://run/{}//{}", app_id, opts.trim())
        }
    } else {
        format!("steam://run/{}", app_id)
    };
    
    println!("[LAUNCH] Initiating game start for AppID: {} with URL: {}", app_id, launch_url);
    
    app_handle.opener().open_url(launch_url, None::<&str>)
        .map_err(|e| format!("Failed to launch game: {}", e))
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
pub async fn search_nexus_mods(app_handle: tauri::AppHandle, offset: u32, search_query: Option<String>) -> Result<Value, String> {
    let api_key = crate::config::get_api_key()?.ok_or("No API key found. Please log in first.")?;
    let config = crate::config::load_config(app_handle.clone()).map_err(|e| e.to_string())?;
    let game_domain = config.game_domain.unwrap_or_else(|| "readyornot".to_string());

    let client = reqwest::Client::new();
    let normalized_query = search_query.unwrap_or_default().trim().to_string();
    let escaped_query = normalized_query
        .replace('\\', "\\\\")
        .replace('\"', "\\\"");
    let base_filter_clause = format!(r#"gameDomainName: [{{ op: EQUALS, value: "{}" }}]"#, game_domain);
    let has_search = !escaped_query.is_empty();
    let filter_clause = if !has_search {
        base_filter_clause.clone()
    } else {
        format!(
            r#"gameDomainName: [{{ op: EQUALS, value: "{}" }}], name: [{{ op: WILDCARD, value: "{}" }}]"#,
            game_domain,
            escaped_query
        )
    };

    let query = format!(
        r#"{{ mods(filter: {{ {} }}, sort: [{{ downloads: {{ direction: DESC }} }}], count: 20, offset: {}) {{ nodes {{ uid modId name summary pictureUrl version author createdAt updatedAt downloads endorsements }} totalCount }} }}"#,
        filter_clause,
        offset
    );

    println!(
        "[SEARCH] offset={}, raw_search_query='{}', escaped_search_query='{}'",
        offset, normalized_query, escaped_query
    );
    println!("[SEARCH] GraphQL query body: {}", query);
    let mut json = execute_graphql_query(&client, &api_key, query).await?;
    println!("[SEARCH] Response keys: {:?}", json.as_object().map(|o| o.keys().collect::<Vec<_>>()));
    if let Some(errors) = json.get("errors") {
        println!("[SEARCH] GraphQL errors: {}", errors);
    }
    let node_count = json
        .get("data")
        .and_then(|d| d.get("mods"))
        .and_then(|m| m.get("nodes"))
        .and_then(|n| n.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    println!("[SEARCH] Got {} mod nodes", node_count);

    if has_search && node_count == 0 {
        let stemmed_query = format!(
            r#"{{ mods(filter: {{ gameDomainName: [{{ op: EQUALS, value: "{}" }}], nameStemmed: [{{ op: EQUALS, value: "{}" }}] }}, sort: [{{ downloads: {{ direction: DESC }} }}], count: 20, offset: {}) {{ nodes {{ uid modId name summary pictureUrl version author createdAt updatedAt downloads endorsements }} totalCount }} }}"#,
            game_domain,
            escaped_query,
            offset
        );
        println!(
            "[SEARCH] Name wildcard returned 0 nodes. Retrying with nameStemmed query: {}",
            stemmed_query
        );
        json = execute_graphql_query(&client, &api_key, stemmed_query).await?;
    }

    let retry_node_count = json
        .get("data")
        .and_then(|d| d.get("mods"))
        .and_then(|m| m.get("nodes"))
        .and_then(|n| n.as_array())
        .map(|a| a.len())
        .unwrap_or(0);

    if has_search && retry_node_count == 0 {
        println!("[SEARCH] Query returned 0 nodes with server-side filters. Falling back to paged browse + local contains filtering.");
        let lowered_query = normalized_query.to_lowercase();
        let mut matched_nodes: Vec<Value> = Vec::new();

        for page in 0..30u32 {
            let page_offset = page * 100;
            let fallback_query = format!(
                r#"{{ mods(filter: {{ {} }}, sort: [{{ updatedAt: {{ direction: DESC }} }}], count: 100, offset: {}) {{ nodes {{ uid modId name summary pictureUrl version author createdAt updatedAt downloads endorsements }} totalCount }} }}"#,
                base_filter_clause, page_offset
            );

            println!("[SEARCH] Fallback page query offset={}", page_offset);
            let page_json = execute_graphql_query(&client, &api_key, fallback_query).await?;
            let page_nodes = page_json
                .get("data")
                .and_then(|d| d.get("mods"))
                .and_then(|m| m.get("nodes"))
                .and_then(|n| n.as_array())
                .cloned()
                .unwrap_or_default();

            if page_nodes.is_empty() {
                break;
            }

            for node in page_nodes {
                let name = node.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                let summary = node.get("summary").and_then(|v| v.as_str()).unwrap_or("").to_lowercase();
                if name.contains(&lowered_query) || summary.contains(&lowered_query) {
                    matched_nodes.push(node);
                }
            }

            if matched_nodes.len() >= 40 {
                break;
            }
        }

        json = serde_json::json!({
            "data": {
                "mods": {
                    "nodes": matched_nodes,
                    "totalCount": matched_nodes.len()
                }
            }
        });
        println!("[SEARCH] Local fallback matched {} nodes", json["data"]["mods"]["totalCount"]);
    }

    if json.get("data").is_none() {
        println!("[SEARCH] No 'data' field in response. Full response: {}", json);
    }
    Ok(json)
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
