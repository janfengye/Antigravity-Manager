use serde::{Deserialize, Serialize};

/// 单个配额桶 (对应 retrieveUserQuotaSummary 里的一个 bucket)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaBucket {
    /// 桶 ID,如 "gemini-weekly" / "gemini-5h" / "3p-weekly" / "3p-5h"
    pub bucket_id: String,
    /// 窗口类型: "weekly" / "5h"
    pub window: String,
    /// 剩余比例 0.0-1.0
    pub remaining_fraction: f64,
    /// 重置时间 (RFC3339)
    pub reset_time: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// 一个模型组 (如 Gemini Models / Claude and GPT models)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaGroup {
    pub display_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub buckets: Vec<QuotaBucket>,
}

/// 模型配额信息
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelQuota {
    pub name: String,
    pub percentage: i32, // 剩余百分比 0-100
    pub reset_time: String,

    // -- 动态参数解析与持久化 --
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_images: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thinking_budget: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recommended: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_output_tokens: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_mime_types: Option<std::collections::HashMap<String, bool>>,
}

/// 配额数据结构
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuotaData {
    pub models: Vec<ModelQuota>,
    pub last_updated: i64,
    #[serde(default)]
    pub is_forbidden: bool,
    /// 禁止访问的原因 (403 详细信息)
    #[serde(default)]
    pub forbidden_reason: Option<String>,
    /// 订阅等级 (FREE/PRO/ULTRA)
    #[serde(default)]
    pub subscription_tier: Option<String>,
    /// 模型淘汰重定向规则表 (old_model_id -> new_model_id)
    #[serde(default)]
    pub model_forwarding_rules: std::collections::HashMap<String, String>,
    /// 按模型组的配额摘要 (weekly + 5h 双窗口),来自 retrieveUserQuotaSummary
    #[serde(default)]
    pub quota_groups: Option<Vec<QuotaGroup>>,
}

impl QuotaData {
    pub fn new() -> Self {
        Self {
            models: Vec::new(),
            last_updated: chrono::Utc::now().timestamp(),
            is_forbidden: false,
            forbidden_reason: None,
            subscription_tier: None,
            model_forwarding_rules: std::collections::HashMap::new(),
            quota_groups: None,
        }
    }

    pub fn add_model(&mut self, model: ModelQuota) {
        self.models.push(model);
    }

    /// 确保当前配额具备有效的订阅等级 (ULTRA/PRO/FREE)
    pub fn ensure_subscription_tier(&mut self) {
        let resolved = resolve_subscription_tier(self.subscription_tier.as_deref(), &self.models);
        self.subscription_tier = Some(resolved);
    }
}

/// 订阅等级标准化：统一规范为 "ULTRA" | "PRO" | "FREE"
pub fn normalize_subscription_tier(tier: &str) -> String {
    let lower = tier.to_lowercase();
    if lower.contains("ultra") {
        "ULTRA".to_string()
    } else if lower.contains("pro") || lower.contains("premium") || lower.contains("advanced") {
        "PRO".to_string()
    } else if lower.contains("free") {
        "FREE".to_string()
    } else {
        tier.to_string()
    }
}

/// 解析/推断订阅等级：结合已有等级与模型列表进行智能兜底
pub fn resolve_subscription_tier(raw_tier: Option<&str>, models: &[ModelQuota]) -> String {
    if let Some(tier) = raw_tier {
        let normalized = normalize_subscription_tier(tier);
        if normalized == "ULTRA" || normalized == "PRO" || normalized == "FREE" {
            return normalized;
        }
    }

    let has_ultra = models
        .iter()
        .any(|m| m.name.to_lowercase().contains("ultra"));
    if has_ultra {
        return "ULTRA".to_string();
    }

    let has_paid_models = models.iter().any(|m| {
        let n = m.name.to_lowercase();
        n.starts_with("claude") || n.starts_with("gpt")
    });
    if has_paid_models {
        return "PRO".to_string();
    }

    "FREE".to_string()
}

impl Default for QuotaData {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_subscription_tier() {
        assert_eq!(normalize_subscription_tier("Google One AI Premium"), "PRO");
        assert_eq!(normalize_subscription_tier("gemini-advanced"), "PRO");
        assert_eq!(normalize_subscription_tier("Gemini Pro"), "PRO");
        assert_eq!(normalize_subscription_tier("pro"), "PRO");
        assert_eq!(normalize_subscription_tier("gemini-ultra"), "ULTRA");
        assert_eq!(normalize_subscription_tier("ULTRA"), "ULTRA");
        assert_eq!(normalize_subscription_tier("free-tier"), "FREE");
        assert_eq!(normalize_subscription_tier("Free"), "FREE");
    }

    #[test]
    fn test_resolve_subscription_tier_model_fallback() {
        let claude_model = ModelQuota {
            name: "claude-3-5-sonnet".to_string(),
            percentage: 100,
            reset_time: "2026-09-17T00:00:00Z".to_string(),
            display_name: None,
            supports_images: None,
            supports_thinking: None,
            thinking_budget: None,
            recommended: None,
            max_tokens: None,
            max_output_tokens: None,
            supported_mime_types: None,
        };
        let flash_model = ModelQuota {
            name: "gemini-2.5-flash".to_string(),
            percentage: 100,
            reset_time: "2026-09-17T00:00:00Z".to_string(),
            display_name: None,
            supports_images: None,
            supports_thinking: None,
            thinking_budget: None,
            recommended: None,
            max_tokens: None,
            max_output_tokens: None,
            supported_mime_types: None,
        };

        // Claude model present without explicit tier -> infer PRO
        assert_eq!(
            resolve_subscription_tier(None, &[flash_model.clone(), claude_model.clone()]),
            "PRO"
        );

        // Flash only without explicit tier -> infer FREE
        assert_eq!(resolve_subscription_tier(None, &[flash_model]), "FREE");

        // Empty models without explicit tier -> infer FREE
        assert_eq!(resolve_subscription_tier(None, &[]), "FREE");

        // Explicit tier overrides model heuristics
        assert_eq!(
            resolve_subscription_tier(Some("Google One AI Premium"), &[]),
            "PRO"
        );
        assert_eq!(
            resolve_subscription_tier(Some("gemini-ultra"), &[claude_model]),
            "ULTRA"
        );
    }
}
