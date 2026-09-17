use crate::proxy::token_manager::ProxyToken;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSpec {
    pub max_output_tokens: Option<u64>,
    pub thinking_budget: Option<u64>,
    pub is_thinking: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SpecsConfig {
    models: HashMap<String, ModelSpec>,
    aliases: HashMap<String, String>,
}

static SPECS: Lazy<SpecsConfig> = Lazy::new(|| {
    let json_str = include_str!("../../resources/model_specs.json");
    serde_json::from_str(json_str).expect("Failed to parse model_specs.json")
});

/// 获取归一化后的模型 ID (基于别名)
pub fn resolve_alias(model_id: &str) -> String {
    SPECS
        .aliases
        .get(model_id)
        .cloned()
        .unwrap_or_else(|| model_id.to_string())
}

/// 获取模型输出 Token 限额 (动态优先)
pub fn get_max_output_tokens(model_id: &str, token: Option<&ProxyToken>) -> u64 {
    let std_id = resolve_alias(model_id);

    // 1. 尝试从账号动态数据中读取
    if let Some(t) = token {
        if let Some(&limit) = t.model_limits.get(&std_id) {
            return limit;
        }
        // 如果原始 ID 没找到，尝试用归一化后的 ID 找
        if let Some(&limit) = t.model_limits.get(model_id) {
            return limit;
        }
    }

    // 2. 回退到静态 JSON
    if let Some(spec) = SPECS.models.get(&std_id) {
        if let Some(limit) = spec.max_output_tokens {
            return limit;
        }
    }

    // 3. 全局兜底
    65535
}

/// 获取思维链预算 (动态优先，根据模型 ID 档位字典自动填充)
pub fn get_thinking_budget(model_id: &str, _token: Option<&ProxyToken>) -> u64 {
    let std_id = resolve_alias(model_id);
    let lower = std_id.to_lowercase();

    // 1. 显式档位匹配（支持 Flash / Pro 的 high, medium, low, extra-low, max 等分级）
    if lower.contains("high") || lower.contains("agent") || lower.contains("max") {
        if lower.contains("pro") {
            return 10001; // Google 官方 gemini-3.1-pro-high / gemini-pro-agent 规范预算
        }
        return 10000; // gemini-3.x-flash-high 满血思考规范预算
    }
    if lower.contains("medium") {
        return 4000; // gemini-3.x-flash-medium 内置逆向规范预算
    }
    if lower.contains("extra-low") {
        return 1000; // gemini-3.5-flash-extra-low 规范预算
    }
    if lower.contains("low") {
        if lower.contains("pro") {
            return 1001; // gemini-3.1-pro-low 规范预算
        }
        return 1000; // gemini-3.x-flash-low 规范预算
    }

    // 2. 静态 JSON 配置 (model_specs.json)
    if let Some(spec) = SPECS.models.get(&std_id) {
        if let Some(budget) = spec.thinking_budget {
            return budget;
        }
    }

    // 3. 所有 Gemini >= 3.0 未带显式后缀的 Flash 模型：
    // medium 或者不带档位后缀，统一默认赋予 4000（内置逆向标准）
    if is_gemini_v3_or_above(&std_id) && lower.contains("flash") {
        return 4000;
    }

    // 4. 传统模型系列默认限额
    if lower.contains("claude") {
        16000
    } else if lower.contains("2.5-flash") || lower.contains("2.0-flash") {
        24576
    } else if lower.contains("pro") {
        49152
    } else {
        24576
    }
}

/// 判断模型是否命中显式启发式档位后缀（-high, -medium, -low, -extra-low, -max, -agent, -thinking 等）
pub fn is_explicit_heuristic_tier_model(model_id: &str) -> bool {
    let std_id = resolve_alias(model_id);
    let lower = std_id.to_lowercase();
    lower.ends_with("-high")
        || lower.ends_with("-medium")
        || lower.ends_with("-low")
        || lower.ends_with("-extra-low")
        || lower.ends_with("-max")
        || lower.ends_with("-thinking")
        || lower.ends_with("-agent")
        || lower.contains("-high-")
        || lower.contains("-medium-")
        || lower.contains("-low-")
        || lower.contains("-extra-low-")
        || lower.contains("-max-")
        || lower.contains("-agent")
        || lower.contains("-thinking")
}

/// 权威解析思维链预算（全协议统一：处理启发式模型强制锁死 vs 裸模型接管客户端 effort）
pub fn resolve_authoritative_thinking_budget(
    model: &str,
    client_effort: Option<&str>,
    _client_budget: Option<u64>,
    token: Option<&ProxyToken>,
) -> u64 {
    // 1. 若为 Gemini < 3 的非思考模型，返回 0
    if is_gemini_under_v3(model) {
        return 0;
    }

    // 2. 启发式模型：绝对最高优先级，根据后缀字典强制返回，彻底忽略客户端 effort
    if is_explicit_heuristic_tier_model(model) {
        return get_thinking_budget(model, token);
    }

    // 3. 非 Gemini 3 系列（例如纯 Claude 或其他传统模型），走传统默认
    if !is_gemini_v3_or_above(model) && !model.to_lowercase().contains("gemini") {
        return get_thinking_budget(model, token);
    }

    // 4. 裸模型（不带显式档位后缀，如 gemini-3-flash, gemini-3.1-pro 等）
    let std_id = resolve_alias(model);
    let lower = std_id.to_lowercase();
    let is_pro = lower.contains("pro");

    // 由客户端 effort / thinkingLevel 接管：
    // high / max / xhigh → 10000 / 10001
    // medium / default / json字段不填 → 4000 / 10001
    // low / extra-low → 1000 / 1001
    // 若客户端试图关闭思考 (none / 0 / disabled) 或未传：绝不关闭思考，强制按 -medium 字典预算填充兜底！
    if let Some(effort) = client_effort {
        let eff_lower = effort.trim().to_lowercase();
        match eff_lower.as_str() {
            "high" | "max" | "xhigh" => {
                if is_pro {
                    10001
                } else {
                    10000
                }
            }
            "low" | "extra-low" => {
                if is_pro {
                    1001
                } else {
                    1000
                }
            }
            "medium" | "default" => {
                if is_pro {
                    10001
                } else {
                    4000
                }
            }
            "none" | "0" | "disabled" => {
                // 试图关闭思考：绝不关闭！强制以 -medium 字典预算填充兜底
                if is_pro {
                    10001
                } else {
                    4000
                }
            }
            _ => {
                // 未知 effort，默认 -medium 字典预算
                if is_pro {
                    10001
                } else {
                    4000
                }
            }
        }
    } else {
        // 客户端未传 effort 或 json 字段不填：默认以 -medium 字典预算填充兜底
        if is_pro {
            10001
        } else {
            4000
        }
    }
}

/// 判断是否为思维模型
#[allow(dead_code)]
pub fn is_thinking_model(model_id: &str) -> bool {
    let std_id = resolve_alias(model_id);
    if let Some(spec) = SPECS.models.get(&std_id) {
        return spec.is_thinking.unwrap_or(false);
    }
    model_id.contains("-thinking") || model_id.contains("thinking")
}

/// 判断是否为 Gemini 且主版本号 < 3.0 的模型（例如 gemini-2.5-flash, gemini-2.5-pro, gemini-2.0-flash, gemini-1.5-pro 等）
/// 此类模型在 Google 官方 API 上不支持 thinkingConfig / 思考参数，严禁注入思考配置，严禁回填思考块与哨兵签名。
pub fn is_gemini_under_v3(model: &str) -> bool {
    let lower = model.to_lowercase();
    if !lower.contains("gemini") {
        return false;
    }
    // 特殊别名/代理模型：gemini-pro-agent / gemini-flash-agent 属于 gemini-3 体系；-exp / thinking-exp 属于官方思维实验模型
    if lower.contains("agent")
        || lower.contains("-exp")
        || lower.contains("thinking-exp")
        || lower.contains("-thinking")
    {
        return false;
    }
    // 检查明确的 gemini-X 形式
    if let Some(idx) = lower.find("gemini-") {
        let rest = &lower[idx + "gemini-".len()..];
        let version_part: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(ver) = version_part.parse::<f32>() {
            return ver < 3.0;
        }
    }
    // 兜底兼容 gemini-1 / gemini-2 形式
    lower.contains("gemini-1") || lower.contains("gemini-2")
}

/// 判断是否为 Gemini 3 及以上版本的模型（例如 gemini-3, gemini-3.1, gemini-3.7, gemini-3.8 等）
/// 此类模型支持并强行/默认开启思考模式。
pub fn is_gemini_v3_or_above(model: &str) -> bool {
    let lower = model.to_lowercase();
    if !lower.contains("gemini") {
        return false;
    }
    if lower.contains("agent") || lower == "gemini-pro" || lower == "gemini-flash" {
        return true;
    }
    if let Some(idx) = lower.find("gemini-") {
        let rest = &lower[idx + "gemini-".len()..];
        let version_part: String = rest
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect();
        if let Ok(ver) = version_part.parse::<f32>() {
            return ver >= 3.0;
        }
    }
    lower.contains("gemini-3") || lower.contains("gemini-4")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gemini_version_checks() {
        assert!(is_gemini_under_v3("gemini-2.5-flash"));
        assert!(is_gemini_under_v3("gemini-2.5-flash-lite"));
        assert!(is_gemini_under_v3("gemini-2.5-pro"));
        assert!(is_gemini_under_v3("gemini-2.0-flash"));
        assert!(is_gemini_under_v3("gemini-1.5-pro"));
        assert!(!is_gemini_under_v3("gemini-3.7-flash-high"));
        assert!(!is_gemini_under_v3("gemini-3-flash"));
        assert!(!is_gemini_under_v3("gemini-3-pro"));
        assert!(!is_gemini_under_v3("gemini-3.1-pro"));
        assert!(!is_gemini_under_v3("gemini-3.8-flash"));
        assert!(!is_gemini_under_v3("gemini-pro-agent"));
        assert!(!is_gemini_under_v3("claude-3-7-sonnet"));

        assert!(!is_gemini_v3_or_above("gemini-2.5-flash"));
        assert!(!is_gemini_v3_or_above("gemini-2.0-flash"));
        assert!(is_gemini_v3_or_above("gemini-3.7-flash-high"));
        assert!(is_gemini_v3_or_above("gemini-3.7-flash"));
        assert!(is_gemini_v3_or_above("gemini-3-flash"));
        assert!(is_gemini_v3_or_above("gemini-3-pro"));
        assert!(is_gemini_v3_or_above("gemini-3.1-pro"));
        assert!(is_gemini_v3_or_above("gemini-3.8-flash"));
        assert!(is_gemini_v3_or_above("gemini-pro-agent"));
        assert!(!is_gemini_v3_or_above("claude-3-7-sonnet"));
    }

    #[test]
    fn test_gemini_thinking_budget() {
        // 显式 -high 后缀模型赋予 10000 满血预算
        assert_eq!(get_thinking_budget("gemini-3.7-flash-high", None), 10000);
        assert_eq!(get_thinking_budget("gemini-3.8-flash-high", None), 10000);
        assert_eq!(get_thinking_budget("gemini-3.9-flash-high", None), 10000);

        // 显式 -medium 后缀模型赋予 4000 预算
        assert_eq!(get_thinking_budget("gemini-3.7-flash-medium", None), 4000);
        assert_eq!(get_thinking_budget("gemini-3.8-flash-medium", None), 4000);

        // 显式 -low 后缀模型赋予 1000 预算
        assert_eq!(get_thinking_budget("gemini-3.7-flash-low", None), 1000);
        assert_eq!(get_thinking_budget("gemini-3.8-flash-low", None), 1000);

        // 未带显式档位后缀的 Gemini >= 3.0 模型，默认赋予 medium 预算 (4000)
        assert_eq!(get_thinking_budget("gemini-3.7-flash", None), 4000);
        assert_eq!(get_thinking_budget("gemini-3.8-flash", None), 4000);
        assert_eq!(get_thinking_budget("gemini-3.9-flash", None), 4000);

        // Pro 系列
        assert_eq!(get_thinking_budget("gemini-3.1-pro-high", None), 10001);
        assert_eq!(get_thinking_budget("gemini-pro-agent", None), 10001);
        assert_eq!(get_thinking_budget("gemini-3.1-pro-low", None), 1001);
    }

    #[test]
    fn test_resolve_authoritative_thinking_budget() {
        // 1. 启发式模型：强制按字典锁定，完全忽略客户端 effort
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.7-flash-high", Some("low"), None, None),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget(
                "gemini-3.7-flash-high",
                Some("none"),
                None,
                None
            ),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro-low", Some("high"), None, None),
            1001
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-pro-agent", Some("low"), None, None),
            10001
        );

        // 2. 裸模型 Flash 系列：接管客户端 effort
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("high"), None, None),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("max"), None, None),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("xhigh"), None, None),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("low"), None, None),
            1000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("extra-low"), None, None),
            1000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("medium"), None, None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("default"), None, None),
            4000
        );

        // 裸模型 Flash 系列：客户端不填或试图关闭，绝不关闭思考，强制回填 -medium (4000)
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", None, None, None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("none"), None, None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("disabled"), None, None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("0"), None, None),
            4000
        );

        // 3. 裸模型 Pro 系列：接管客户端 effort
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", Some("high"), None, None),
            10001
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", Some("low"), None, None),
            1001
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", Some("medium"), None, None),
            10001
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", None, None, None),
            10001
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", Some("none"), None, None),
            10001
        );

        // 4. Gemini < 3 非思考模型：返回 0
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-2.5-flash", Some("high"), None, None),
            0
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-1.5-pro", None, None, None),
            0
        );

        // 5. 裸模型传入思考预算字段 (client_budget) 直接被彻底忽略，完全由客户端等级或服务器兜底接管
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", Some("high"), Some(1024), None),
            10000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", None, Some(1024), None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3-flash", None, Some(32000), None),
            4000
        );
        assert_eq!(
            resolve_authoritative_thinking_budget("gemini-3.1-pro", None, Some(1000), None),
            10001
        );
    }
}
