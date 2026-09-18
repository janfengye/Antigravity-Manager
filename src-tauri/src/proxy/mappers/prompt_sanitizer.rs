//! Prompt Sanitizer Pipeline Module
//!
//! 提供中转报文生成后、发往上游出站前的提示词专用清洗流水线节点。
//! 严格且精准地剥离第三方客户端（Claude Code CLI / Cherry Studio 等）
//! 在 system / user 块中注入的计费元数据伪 Header（如 `x-anthropic-billing-header` 与
//! `cc_version` / `cc_entrypoint` 签名），防止触发 Google Cloud Code 上游 WAF
//! 的特征拦截（虚假 429 RESOURCE_EXHAUSTED）进而导致全局账号池 503 级联封锁。
//!
//! 核心设计与防误杀铁律：
//! 1. 深度保护代码块：自动提取并保护所有围栏代码块（```）与行内代码（`），代码内容 100% 豁免；
//! 2. 严禁碰触会话唯一性（Zero Session Touch）：绝不清洗或过滤任何 session 相关字段（包括
//!    `x-jeikcode-session-id`, `x-atomcode-session-id`, `x-session-id` 等）；会话唯一性由
//!    `SessionScope` 按照严格优先级链条独立处理，本模块对所有 session 标识保持 100% 原样透传；
//! 3. 严禁碰触用户自定义分割符与标签（Zero Delimiter Touch）：绝不删除 `=== ... ===`、`--- ... ---`
//!    以及 XML 标签（如 `<environment>`, `<workflow_and_execution_discipline>` 等），保证如
//!    JeikCode / AtomCode 等 AI 编码框架的用户与系统指令结构完好无损；
//! 4. 严禁裁剪或假定 IDE 反刍文本：不预设删除任何用户自然语言与业务指令；
//! 5. 全链路审计一致：中转报文生成并填充完思考块后统一介入，保证写进数据库与转出的报文真实一致。

use once_cell::sync::Lazy;
use regex::Regex;
use serde_json::Value;

/// 代码块保护正则：隔离多行围栏代码块 ```...``` 与行内反引号 `...`
static RE_CODE_BLOCK: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?ms)(```[\s\S]*?```|`[^`\r\n]+`)").unwrap());

/// 触发上游 Google WAF 拦截的高危客户端伪 Header 正则（仅在非代码区域生效）：
/// 1) 通用泛化匹配各类客户端注入的 `*-billing*` 伪 Header 行（如 `x-anthropic-billing-header:`, `x-billing:`, `x-client-billing:`, `anthropic-billing-header:` 等）
/// 2) 匹配任何以 `x-` 开头且携带 Claude Code CLI 计费签名特征（`cc_version`, `cc_entrypoint`）的伪 Header
/// 严格排除任何 `session` 关键字，确保用户提问与会话跟踪完全免受干扰。
static RE_WAF_TRIGGER_HEADERS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(concat!(
        r"(?im)^\s*(?:x-[a-z0-9_-]*billing[a-z0-9_-]*|[a-z0-9_-]+-billing-(?:header|metadata|token|info)):\s*[^\r\n]*(\r?\n)?",
        r"|^\s*x-[a-z0-9_-]+:\s*[^\r\n]*(?:cc_version|cc_entrypoint)[^\r\n]*(\r?\n)?"
    ))
    .unwrap()
});

/// 连续多余空行收拢正则（剥离元数据后若产生 3 个及以上连续换行，收拢为 2 个换行以保持自然段落）
static RE_MULTI_NEWLINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{3,}").unwrap());

pub struct PromptSanitizer;

impl PromptSanitizer {
    /// 对单段提示词文本进行通用、高精度的净化：
    /// 1. 快速检查是否包含高危 WAF 拦截触发词（`billing`, `cc_version`, `cc_entrypoint`），未包含直接原样返回；
    /// 2. 提取并保护所有代码块，使其免受任何正则影响；
    /// 3. 仅剥离触发 WAF 的客户端计费伪 Header，绝不修改任何会话 ID、XML 标签或 `=== ... ===` 分隔符；
    /// 4. 完整恢复受保护的代码块；
    /// 5. 规范化换行，保留所有多行正文排版。
    pub fn clean_text(text: &str) -> String {
        // 快速前置检查：若文本不包含高危特征，直接原样返回，零开销
        let lower = text.to_lowercase();
        let has_suspect = lower.contains("billing")
            || lower.contains("cc_version")
            || lower.contains("cc_entrypoint");

        if !has_suspect {
            return text.to_string();
        }

        // 步骤 1：保护代码块
        let mut placeholders: Vec<String> = Vec::new();
        let protected_text = RE_CODE_BLOCK.replace_all(text, |caps: &regex::Captures| {
            let idx = placeholders.len();
            placeholders.push(caps[0].to_string());
            format!("__PROMPT_SANITIZER_CODE_BLOCK_{}__", idx)
        });

        // 步骤 2：对非代码区域仅剥离触发 WAF 的高危伪 Header
        let pass1 = RE_WAF_TRIGGER_HEADERS.replace_all(&protected_text, "");

        // 步骤 3：恢复受保护的代码块
        let mut restored = pass1.into_owned();
        for (idx, original_code) in placeholders.iter().enumerate() {
            let ph = format!("__PROMPT_SANITIZER_CODE_BLOCK_{}__", idx);
            restored = restored.replace(&ph, original_code);
        }

        // 步骤 4：连续空行收拢（若因删除单行产生多余换行，保持正常双换行段落结构）
        let normalized = RE_MULTI_NEWLINE.replace_all(&restored, "\n\n");

        normalized.trim().to_string()
    }

    /// 清洗 parts 数组中的所有文本节点
    pub fn sanitize_parts(parts: &mut Vec<Value>) -> usize {
        let mut cleaned_count = 0;
        for part in parts.iter_mut() {
            if let Some(obj) = part.as_object_mut() {
                if let Some(text_val) = obj.get("text").and_then(Value::as_str) {
                    let cleaned = Self::clean_text(text_val);
                    if cleaned != text_val {
                        obj.insert("text".to_string(), Value::String(cleaned));
                        cleaned_count += 1;
                    }
                }
            }
        }
        cleaned_count
    }

    /// 核心流水线节点：清洗统一中转报文中的系统提示词（systemInstruction）与对话流（contents）
    /// 支持顶层 Gemini Body 以及包裹在 `request` 字段下的 Body。
    pub fn sanitize_gemini_payload(body: &mut Value) -> usize {
        let mut total_cleaned = 0;

        // 兼容处理：若存在包装层 "request"，清洗包装内部
        if let Some(inner) = body.get_mut("request").and_then(Value::as_object_mut) {
            let mut inner_val = Value::Object(inner.clone());
            let count = Self::sanitize_gemini_payload_inner(&mut inner_val);
            if let Value::Object(new_inner) = inner_val {
                *inner = new_inner;
            }
            total_cleaned += count;
        }

        // 清洗当前层
        total_cleaned += Self::sanitize_gemini_payload_inner(body);
        total_cleaned
    }

    fn sanitize_gemini_payload_inner(body: &mut Value) -> usize {
        let mut cleaned_count = 0;

        // 1. 清洗系统提示词 (systemInstruction)
        if let Some(sys) = body
            .get_mut("systemInstruction")
            .and_then(Value::as_object_mut)
        {
            if let Some(parts) = sys.get_mut("parts").and_then(Value::as_array_mut) {
                cleaned_count += Self::sanitize_parts(parts);
            }
        }

        // 2. 清洗所有对话轮次 (contents，包括 user 块与 model 块)
        if let Some(contents) = body.get_mut("contents").and_then(Value::as_array_mut) {
            for turn in contents.iter_mut() {
                if let Some(parts) = turn.get_mut("parts").and_then(Value::as_array_mut) {
                    cleaned_count += Self::sanitize_parts(parts);
                }
            }
        }

        cleaned_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_clean_text_multiline_system_prompt_with_billing() {
        let raw = concat!(
            "x-anthropic-billing-header: cc_version=2.1.220.04c; cc_entrypoint=sdk-ts;\n",
            "You are Claude Code, Anthropic's official CLI for Claude.\n\n",
            "Please follow these instructions:\n",
            "1. Assist with coding tasks."
        );
        let cleaned = PromptSanitizer::clean_text(raw);
        assert!(!cleaned.contains("x-anthropic-billing-header"));
        assert!(cleaned.starts_with("You are Claude Code"));
        assert!(cleaned.contains("1. Assist with coding tasks."));
    }

    #[test]
    fn test_clean_waf_trigger_cc_entrypoint_header() {
        let raw = concat!(
            "x-custom-billing: cc_version=2.0; cc_entrypoint=cli;\n",
            "Actual user instructions."
        );
        let cleaned = PromptSanitizer::clean_text(raw);
        assert_eq!(cleaned, "Actual user instructions.");
    }

    #[test]
    fn test_clean_generic_wildcard_billing_headers() {
        let raw = concat!(
            "x-billing: enabled\n",
            "x-custom-billing-info: token123\n",
            "x-client-billing: active\n",
            "anthropic-billing-header: cc_version=2.1\n",
            "Actual user instructions."
        );
        let cleaned = PromptSanitizer::clean_text(raw);
        assert_eq!(cleaned, "Actual user instructions.");
    }

    #[test]
    fn test_strictly_preserves_session_headers_and_tokens() {
        // 关键验证：任何形式的 session ID 绝不能被破坏或删除
        let raw = concat!(
            "x-jeikcode-session-id: session-abc-123\n",
            "x-atomcode-session-id: atom-sess-456\n",
            "x-session-id: generic-sess-789\n",
            "x-client-session-id: client-sess-000\n",
            "Please keep my session active."
        );
        let cleaned = PromptSanitizer::clean_text(raw);
        assert_eq!(cleaned, raw);
    }

    #[test]
    fn test_strictly_preserves_user_delimiters_and_xml_tags() {
        // 关键验证：用户的 JeikCode / AtomCode 提示词格式包含 XML 与 === ... === 分割符，100% 原样保留
        let jeik_prompt = concat!(
            "<environment>\n",
            "You are JeikCode AI coding Agent by Jeik.\n\n",
            "## PRECEDENCE:\n",
            "- Content enclosed in XML tags represents current environment.\n",
            "- Rules under headers matching `=== ... (*.md) ===` (such as `AGENTS.md`, `CLAUDE.md`, `=== MEMORY ===`) constitute USER PROVISIONS.\n",
            "</environment>\n\n",
            "=== AGENTS.md ===\n",
            "User custom provisions.\n",
            "=== MEMORY ===\n",
            "Memory block 1."
        );
        let cleaned = PromptSanitizer::clean_text(jeik_prompt);
        assert_eq!(cleaned, jeik_prompt);
    }

    #[test]
    fn test_protects_code_blocks_containing_waf_signatures() {
        let code = concat!(
            "Here is my code:\n",
            "```python\n",
            "headers = {'x-anthropic-billing-header': 'cc_version=1.0'}\n",
            "print(headers)\n",
            "```\n",
            "Does this look right?"
        );
        let cleaned = PromptSanitizer::clean_text(code);
        assert_eq!(cleaned, code);
    }

    #[test]
    fn test_protects_normal_user_prompts_and_http_headers() {
        let normal_text = concat!(
            "How do I set Authorization: Bearer <token> in curl?\n",
            "- Step 1: Add -H flag\n",
            "- Step 2: Test endpoint\n\n",
            "Total steps: 2"
        );
        let cleaned = PromptSanitizer::clean_text(normal_text);
        assert_eq!(cleaned, normal_text);
    }

    #[test]
    fn test_sanitize_gemini_payload_system_and_user_blocks() {
        let mut payload = json!({
            "project": "test-project",
            "model": "gemini-3.8-flash-high",
            "request": {
                "systemInstruction": {
                    "role": "user",
                    "parts": [
                        {
                            "text": "x-anthropic-billing-header: cc_version=2.1;\n<environment>You are JeikCode</environment>\n=== AGENTS.md ==="
                        }
                    ]
                },
                "contents": [
                    {
                        "role": "user",
                        "parts": [
                            {
                                "text": "x-anthropic-billing-header: cc_entrypoint=cli;\nPlease analyze my data:\n=== MEMORY ===\n- Metric A: 10\n- Metric B: 20"
                            }
                        ]
                    },
                    {
                        "role": "model",
                        "parts": [
                            {
                                "text": "Model output response."
                            }
                        ]
                    }
                ]
            }
        });

        let cleaned_count = PromptSanitizer::sanitize_gemini_payload(&mut payload);
        assert_eq!(cleaned_count, 2);

        let sys_text = payload["request"]["systemInstruction"]["parts"][0]["text"]
            .as_str()
            .unwrap();
        // 计费头被精确移除，但 XML 与 === AGENTS.md === 完好无损
        assert!(!sys_text.contains("x-anthropic-billing-header"));
        assert!(sys_text.contains("<environment>You are JeikCode</environment>"));
        assert!(sys_text.contains("=== AGENTS.md ==="));

        let user_text = payload["request"]["contents"][0]["parts"][0]["text"]
            .as_str()
            .unwrap();
        assert!(!user_text.contains("x-anthropic-billing-header"));
        assert!(user_text.contains("=== MEMORY ==="));
        assert!(user_text.contains("- Metric A: 10\n- Metric B: 20"));
    }
}
