const GOOGLE_VARS: [&str; 3] = ["GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET", "GOOGLE_API_KEY"];

fn unquote(value: &str) -> &str {
    for q in ['"', '\''] {
        if value.len() >= 2 && value.starts_with(q) && value.ends_with(q) {
            return &value[1..value.len() - 1];
        }
    }
    value
}

fn main() {
    if std::path::Path::new("../.env").exists() {
        println!("cargo:rerun-if-changed=../.env");
    }
    for name in GOOGLE_VARS {
        println!("cargo:rerun-if-env-changed={name}");
    }

    if let Ok(text) = std::fs::read_to_string("../.env") {
        for name in GOOGLE_VARS {
            if std::env::var_os(name).is_some() {
                continue;
            }
            for line in text.lines() {
                if let Some((key, value)) = line.split_once('=') {
                    if key.trim() == name {
                        println!("cargo:rustc-env={name}={}", unquote(value.trim()));
                    }
                }
            }
        }
    }

    tauri_build::build()
}
