use serde_json;
use std::fs;

use super::account::get_data_dir;
use crate::models::AppConfig;
use tracing::warn;

const CONFIG_FILE: &str = "gui_config.json";

/// Load application configuration
pub fn load_app_config() -> Result<AppConfig, String> {
    let data_dir = get_data_dir()?;
    let config_path = data_dir.join(CONFIG_FILE);

    if !config_path.exists() {
        let config = AppConfig::new();
        // [FIX #1460] Persist initial config to prevent new API Key on every refresh
        let _ = save_app_config(&config);
        return Ok(config);
    }

    let content = fs::read_to_string(&config_path)
        .map_err(|e| format!("failed_to_read_config_file: {}", e))?;

    let mut v: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| format!("failed_to_parse_config_file: {}", e))?;

    let mut modified = false;

    // Migration logic
    if let Some(proxy) = v.get_mut("proxy") {
        // [FIX #1738] Enhanced type checking for custom_mapping
        // Ensures the field is always parsed as an object, preventing type mismatch errors
        let mut custom_mapping = match proxy.get("custom_mapping") {
            Some(m) if m.is_object() => m.as_object().unwrap().clone(),
            Some(m) => {
                // If custom_mapping is not an object type (e.g., string), log warning and reset to empty
                tracing::warn!(
                    "Invalid custom_mapping type (expected object, got {:?}), resetting to empty",
                    m
                );
                serde_json::Map::new()
            }
            None => serde_json::Map::new(),
        };

        // Migrate Anthropic mapping
        if let Some(anthropic) = proxy
            .get_mut("anthropic_mapping")
            .and_then(|m| m.as_object_mut())
        {
            for (k, v) in anthropic.iter() {
                // Only move non-series fields, as series fields are now handled by Preset logic or builtin tables
                if !k.ends_with("-series") {
                    if !custom_mapping.contains_key(k) {
                        custom_mapping.insert(k.clone(), v.clone());
                    }
                }
            }
            // Remove old field
            proxy.as_object_mut().unwrap().remove("anthropic_mapping");
            modified = true;
        }

        // Migrate OpenAI mapping
        if let Some(openai) = proxy
            .get_mut("openai_mapping")
            .and_then(|m| m.as_object_mut())
        {
            for (k, v) in openai.iter() {
                if !k.ends_with("-series") {
                    if !custom_mapping.contains_key(k) {
                        custom_mapping.insert(k.clone(), v.clone());
                    }
                }
            }
            // Remove old field
            proxy.as_object_mut().unwrap().remove("openai_mapping");
            modified = true;
        }

        // 预设无后缀 3.6+ Flash 模型到 Tiered 自适应模型的默认映射规则
        // 自动注入到用户的自定义模型列表中，用户可在 UI 界面查阅、删除或自定义修改保存；默认按此预设执行
        for (k, v) in [
            ("gemini-3.6-flash", "gemini-3.6-flash-tiered"),
            ("gemini-3.7-flash", "gemini-3.7-flash-tiered"),
            ("gemini-3.8-flash", "gemini-3.8-flash-tiered"),
            ("gemini-3.x-flash", "3.x-flash-tiered"),
        ] {
            if !custom_mapping.contains_key(k) {
                custom_mapping.insert(k.to_string(), serde_json::Value::String(v.to_string()));
                modified = true;
            }
        }

        // Migrate log retention max_disk_mb: if 0, smoothly recover to 1024 MiB default
        if let Some(log_retention) = proxy
            .get_mut("log_retention")
            .and_then(|m| m.as_object_mut())
        {
            if let Some(max_disk_mb) = log_retention.get("max_disk_mb").and_then(|v| v.as_u64()) {
                if max_disk_mb == 0 {
                    log_retention.insert("max_disk_mb".to_string(), serde_json::Value::from(1024));
                    modified = true;
                }
            }
        }

        // Migrate legacy User-Agent in user_agent_override and saved_user_agent to >= 4.3.0
        // to prevent upstream Google 404/429 model rejections
        for ua_field in ["user_agent_override", "saved_user_agent"] {
            if let Some(ua_val) = proxy.get(ua_field).and_then(|v| v.as_str()) {
                let sanitized = crate::constants::sanitize_egress_user_agent(ua_val);
                if sanitized != ua_val {
                    tracing::info!(
                        field = %ua_field,
                        old = %ua_val,
                        new = %sanitized,
                        "Migrating legacy User-Agent config to supported stable floor"
                    );
                    proxy
                        .as_object_mut()
                        .unwrap()
                        .insert(ua_field.to_string(), serde_json::Value::String(sanitized));
                    modified = true;
                }
            }
        }

        if modified {
            proxy.as_object_mut().unwrap().insert(
                "custom_mapping".to_string(),
                serde_json::Value::Object(custom_mapping),
            );
        }
    }

    let config: AppConfig = serde_json::from_value(v)
        .map_err(|e| format!("failed_to_convert_config_after_migration: {}", e))?;

    // If migration occurred, auto-save once to clean up the file
    if modified {
        let _ = save_app_config(&config);
    }

    Ok(config)
}

/// Save application configuration (atomic write)
pub fn save_app_config(config: &AppConfig) -> Result<(), String> {
    let data_dir = get_data_dir()?;
    let config_path = data_dir.join(CONFIG_FILE);

    let content = serde_json::to_string_pretty(config)
        .map_err(|e| format!("failed_to_serialize_config: {}", e))?;

    crate::utils::fs::write_atomic(&config_path, content.as_bytes())
        .map_err(|e| format!("failed_to_save_config: {}", e))
}
