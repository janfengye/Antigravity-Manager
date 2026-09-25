use super::policy::ProxyProtocol;
use serde_json::{json, Value};

/// 客户端思考控制开关（三态枚举）
/// 遵循最高指令：思考开关（一票否决权） > 思考等级 > 思考预算
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientThinkingSwitch {
    /// 显式关闭（最高准则，一票否决：disabled / 0 / none / off）
    Disabled,
    /// 显式开启（enabled / on / 具体档位 / 具体预算）
    Enabled,
    /// 缺省（未传任何思考参数，或值为 default；业务铁律：缺省就是默认开）
    Default,
}

impl ClientThinkingSwitch {
    /// 是否允许开启思考（缺省即开，关则一票否决）
    pub fn is_allowed(self) -> bool {
        matches!(self, Self::Enabled | Self::Default)
    }

    /// 是否显式关闭
    pub fn is_disabled(self) -> bool {
        matches!(self, Self::Disabled)
    }
}

/// 统一归一化提取客户端思考开关状态（适用于 OpenAI / Claude / Gemini / Codex 等所有协议）
pub fn extract_client_thinking_switch(
    thinking_type: Option<&str>,
    budget: Option<u64>,
    effort: Option<&str>,
) -> ClientThinkingSwitch {
    let t_type = thinking_type.map(|s| s.trim().to_lowercase());
    let eff = effort.map(|s| s.trim().to_lowercase());

    // 1. 显式关闭判定（最高优先级，一票否决）
    if matches!(
        t_type.as_deref(),
        Some("disabled") | Some("off") | Some("false") | Some("0")
    ) || budget == Some(0)
        || matches!(
            eff.as_deref(),
            Some("none") | Some("off") | Some("false") | Some("0") | Some("disabled")
        )
    {
        return ClientThinkingSwitch::Disabled;
    }

    // 2. 显式开启判定（只要客户端带了有效开启标记、具体预算，或任何非空/非关闭的思考等级）
    if matches!(
        t_type.as_deref(),
        Some("enabled") | Some("on") | Some("true")
    ) || budget.map_or(false, |b| b > 0)
        || eff.as_deref().map_or(false, |e| {
            !e.is_empty()
                && e != "none"
                && e != "off"
                && e != "false"
                && e != "0"
                && e != "disabled"
                && e != "default"
        })
    {
        return ClientThinkingSwitch::Enabled;
    }

    // 3. 缺省（全未传，或仅为 "default"；铁律：缺省就是默认开）
    ClientThinkingSwitch::Default
}

/// 统一进站思考管线（InboundThinkingPipeline）
/// 接收任何协议转译成的 Google contents 统一报文，单向流转执行：
/// 1. 协议策略签名清洗 (Chat 协议丢弃客户端签名，其他协议验签)
/// 2. 思考块首位强制排序与占位符规范化
/// 3. 状态机历史思维链无损复活 (Hydration)
/// 4. 终审脱敏与前缀缓存格式规范化 (Finalize)
pub struct InboundThinkingPipeline;

impl InboundThinkingPipeline {
    /// 执行统一进站处理
    pub fn process_contents(
        contents: &mut Vec<Value>,
        protocol: ProxyProtocol,
        target_model: &str,
        is_thinking_enabled: bool,
        session_id: Option<&str>,
        is_retry: bool,
    ) {
        let trusts_signature = protocol.trusts_client_signature();
        let is_claude = target_model.to_lowercase().contains("claude");

        // 0. 统一上下文结构对齐（Pipeline First 统一治理）：
        // 将连续的 user 消息直到下一个 model，统一合并为一个 user 轮次的多个 block (parts)，保持严格顺序。
        // 这彻底消除了 Adapter 层各自为政导致的轮次错位，使得四大协议进入流水线后结构 100% 同构！
        let mut normalized_contents = Vec::with_capacity(contents.len());
        for msg in contents.drain(..) {
            let is_user = msg.get("role").and_then(|r| r.as_str()) == Some("user");
            let prev_is_user = normalized_contents
                .last()
                .and_then(|last: &Value| last.get("role").and_then(|r| r.as_str()))
                == Some("user");
            if is_user && prev_is_user {
                let last = normalized_contents.last_mut().unwrap();
                if let (Some(last_parts), Some(msg_parts)) = (
                    last.get_mut("parts").and_then(|p| p.as_array_mut()),
                    msg.get("parts").and_then(|p| p.as_array()),
                ) {
                    last_parts.extend(msg_parts.iter().cloned());
                    continue;
                }
            }
            normalized_contents.push(msg);
        }
        *contents = normalized_contents;

        // 预先计算每一轮的前置因果锚点 (causal anchor)，以便无 ID 的 Gemini 原生工具调用也能无损合成确定性 ID
        let anchors: Vec<String> = (0..contents.len())
            .map(|i| {
                let preceding = if i > 0 { contents.get(i - 1) } else { None };
                crate::proxy::thinking_store::compute_causal_anchor(preceding)
            })
            .collect();

        // 1. 协议策略清洗与位置规范化
        for (msg_idx, content) in contents.iter_mut().enumerate() {
            let anchor = &anchors[msg_idx];
            let is_model = matches!(
                content.get("role").and_then(|r| r.as_str()),
                Some("model") | Some("assistant")
            );

            if let Some(parts) = content.get_mut("parts").and_then(|p| p.as_array_mut()) {
                if is_model {
                    let mut thinking_part = None;
                    let mut extra_thinking_parts = Vec::new();
                    let mut other_parts = Vec::new();
                    let mut fc_counter = 0usize;

                    for mut part in parts.drain(..) {
                        let is_thought = part
                            .get("thought")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false)
                            || (part.get("thoughtSignature").is_some()
                                && part.get("functionCall").is_none()
                                && part.get("functionResponse").is_none());

                        if is_thought {
                            let text = part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                            let is_placeholder =
                                crate::proxy::thinking_store::is_placeholder_thought(text);

                            // 校验客户端签名有效性与模型兼容性
                            let mut effective_sig = None;
                            if let Some(sig) = part
                                .get("thoughtSignature")
                                .or_else(|| part.get("thought_signature"))
                                .or_else(|| part.get("signature"))
                                .and_then(|s| s.as_str())
                            {
                                if sig == crate::proxy::thinking_store::SENTINEL_SIGNATURE {
                                    // Claude 模型绝不接受 Gemini 哨兵签名，避免触发 400 Invalid signature
                                    if !is_claude {
                                        effective_sig = Some(sig.to_string());
                                    }
                                } else if trusts_signature && sig.len() >= 50 {
                                    let cached_family = crate::proxy::SignatureCache::global()
                                        .get_signature_family(sig);
                                    let compatible = match cached_family {
                                        Some(family) => {
                                            crate::proxy::mappers::common_utils::is_model_compatible(
                                                &family,
                                                target_model,
                                            ) || (is_claude
                                                && family.to_lowercase().contains("claude"))
                                        }
                                        None => {
                                            if target_model.to_lowercase().contains("gemini") {
                                                crate::proxy::thinking_store::is_likely_gemini_signature(sig)
                                            } else if is_claude {
                                                crate::proxy::thinking_store::is_claude_signature(
                                                    sig,
                                                )
                                            } else {
                                                true
                                            }
                                        }
                                    };
                                    if compatible {
                                        let final_sig = if is_claude {
                                            crate::proxy::thinking_store::ensure_google_claude_thought_signature(sig)
                                        } else {
                                            sig.to_string()
                                        };
                                        effective_sig = Some(final_sig);
                                    } else if target_model.to_lowercase().contains("gemini") {
                                        tracing::warn!(
                                            "[InboundPipeline] Stripping foreign signature (len: {}) from thought block for Gemini model {}",
                                            sig.len(), target_model
                                        );
                                        effective_sig = None;
                                    } else if is_claude {
                                        tracing::warn!(
                                            "[InboundPipeline] Stripping foreign signature (len: {}) from thought block for Claude model {}",
                                            sig.len(), target_model
                                        );
                                        effective_sig = None;
                                    }
                                }
                            }

                            // 保留真实原始思考文本的尾部换行与空白，绝不进行破坏性 trim，保证与上一轮流式输出字节级严格一致
                            let final_thought_text = if (is_placeholder || text.trim().is_empty())
                                && effective_sig.is_none()
                            {
                                "..."
                            } else {
                                text
                            };

                            let mut thought_obj = json!({
                                "text": final_thought_text,
                                "thought": true,
                            });
                            if let Some(sig) = effective_sig {
                                thought_obj["thoughtSignature"] = json!(sig);
                            }

                            if thinking_part.is_none() {
                                thinking_part = Some(thought_obj);
                            } else {
                                // 多个思考块时，非首位的多余思考块降级为普通文本
                                if !final_thought_text.is_empty() && final_thought_text != "..." {
                                    extra_thinking_parts
                                        .push(json!({ "text": final_thought_text }));
                                }
                            }
                        } else {
                            if target_model.to_lowercase().contains("gemini") {
                                // 协议无关全局工具签名回填：若当前部件为 functionCall 且尚未携带签名，优先按显式 ID 或因果合成 ID 查询 SignatureCache
                                if let Some(fc) = part.get("functionCall") {
                                    let needs_real_sig = part
                                        .get("thoughtSignature")
                                        .and_then(|s| s.as_str())
                                        .map_or(true, |s| {
                                            s == crate::proxy::thinking_store::SENTINEL_SIGNATURE
                                        });
                                    if needs_real_sig {
                                        let name = fc
                                            .get("name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("unknown");
                                        let synthetic_id =
                                            crate::proxy::thinking_store::synthesize_tool_id(
                                                name,
                                                fc.get("args"),
                                                anchor,
                                                fc_counter,
                                            );
                                        fc_counter += 1;

                                        // 核心原则：统一根据上下文因果合成伪 ID 查库回填，不受客户端是否携带或使用何种 tool_id 限制
                                        let found_sig = crate::proxy::SignatureCache::global()
                                            .get_tool_signature(&synthetic_id)
                                            .or_else(|| {
                                                fc.get("id")
                                                    .and_then(|id| id.as_str())
                                                    .filter(|s| !s.trim().is_empty())
                                                    .and_then(|id| {
                                                        crate::proxy::SignatureCache::global()
                                                            .get_tool_signature(id)
                                                    })
                                            });

                                        if let Some(sig) = found_sig {
                                            if crate::proxy::thinking_store::is_likely_gemini_signature(&sig)
                                            {
                                                part["thoughtSignature"] = json!(sig);
                                            }
                                        }
                                    }
                                }

                                if let Some(fc_sig) =
                                    part.get("thoughtSignature").and_then(|s| s.as_str())
                                {
                                    if !crate::proxy::thinking_store::is_likely_gemini_signature(
                                        fc_sig,
                                    ) {
                                        tracing::warn!(
                                            "[InboundPipeline] Replacing foreign functionCall thoughtSignature (len: {}) with sentinel for Gemini",
                                            fc_sig.len()
                                        );
                                        part["thoughtSignature"] =
                                            json!(crate::proxy::thinking_store::SENTINEL_SIGNATURE);
                                    }
                                }
                            } else if is_claude {
                                if let Some(obj) = part.as_object_mut() {
                                    obj.remove("thoughtSignature");
                                    obj.remove("thought_signature");
                                }
                            }
                            // 非思考部件：可能是普通正文/过程进度说明（commentary），也可能是 functionCall 等
                            let is_plain_text = part.get("text").is_some()
                                && part.get("functionCall").is_none()
                                && part.get("functionResponse").is_none();

                            if is_plain_text {
                                let raw_text =
                                    part.get("text").and_then(|v| v.as_str()).unwrap_or("");
                                if raw_text.trim().is_empty() {
                                    // 丢弃纯空白文本部件，避免触发 Gemini 400 校验或破坏前缀缓存哈希稳定性
                                    continue;
                                }

                                // 跨家族协议自愈：检查是否夹带 <think> 标签包裹的思考内容（如从 Claude 跨切回 Gemini）
                                if let Some((extracted_thought, clean_visible)) =
                                    crate::proxy::thinking_store::extract_think_tags(raw_text)
                                {
                                    if thinking_part.is_none() && is_thinking_enabled {
                                        let final_thought = if extracted_thought.is_empty() {
                                            "..."
                                        } else {
                                            extracted_thought.as_str()
                                        };
                                        let mut t_obj = json!({
                                            "text": final_thought,
                                            "thought": true,
                                        });
                                        if !is_claude {
                                            t_obj["thoughtSignature"] = json!(
                                                crate::proxy::thinking_store::SENTINEL_SIGNATURE
                                            );
                                        }
                                        thinking_part = Some(t_obj);
                                    }
                                    if !clean_visible.is_empty() {
                                        other_parts.push(json!({ "text": clean_visible }));
                                    }
                                    continue;
                                }

                                // 协议无关自愈：检查是否夹带旧版遗留思考前缀 (如 **Thinking**)
                                if raw_text.trim_start().starts_with("**Thinking**") {
                                    let clean_thought = Self::strip_thinking_prefix(raw_text);
                                    if thinking_part.is_none() {
                                        // 历史无原生思考块时，将遗留思考文字提炼为合法的首位思考块
                                        let final_thought = if clean_thought.trim().is_empty() {
                                            "..."
                                        } else {
                                            &clean_thought
                                        };
                                        let mut t_obj = json!({
                                            "text": final_thought,
                                            "thought": true,
                                        });
                                        if !is_claude {
                                            t_obj["thoughtSignature"] = json!(
                                                crate::proxy::thinking_store::SENTINEL_SIGNATURE
                                            );
                                        }
                                        thinking_part = Some(t_obj);
                                    }
                                    // 若已有思考块，该遗留思考块作为陈旧副本剥离，防止二次污染正文
                                    continue;
                                }

                                other_parts.push(part);
                            } else {
                                other_parts.push(part);
                            }
                        }
                    }

                    // 核心前缀保序：首位强制存在且仅存在一个 thinking_part，其余正文与工具调用紧随其后
                    let mut new_parts = Vec::with_capacity(parts.len() + 1);
                    if let Some(tp) = thinking_part {
                        new_parts.push(tp);
                    }
                    new_parts.extend(extra_thinking_parts);
                    new_parts.extend(other_parts);
                    *parts = new_parts;
                } else {
                    // role == "user" 的通用进站治理：多模态工具响应 (functionResponse) 深度解构
                    // 确保全协议 (OpenAI / Claude / Gemini / Responses) 的工具结果中夹带的图片均被提升为独立的 inlineData 视觉感知输入
                    let mut extra_inline_parts = Vec::new();
                    for part in parts.iter_mut() {
                        if let Some(fr) = part.get_mut("functionResponse") {
                            if let Some(resp) = fr.get_mut("response") {
                                for key in ["result", "output"] {
                                    if let Some(v) = resp.get_mut(key) {
                                        if let Some(s) = v.as_str() {
                                            if s.contains("data:image/") {
                                                let clean_s = crate::proxy::mappers::common_utils::extract_multimodal_from_tool_text(s, &mut extra_inline_parts);
                                                *v = json!(clean_s);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                    parts.extend(extra_inline_parts);
                }
            }
        }

        // 2. 状态机无损复活 (Hydration)
        // 开思考时全局复活；关思考时若包含工具调用，亦执行复活以取回历史工具防伪签名
        let has_function_call = contents.iter().any(|c| {
            c.get("parts")
                .and_then(|p| p.as_array())
                .map_or(false, |parts| {
                    parts.iter().any(|p| p.get("functionCall").is_some())
                })
        });
        if (is_thinking_enabled || has_function_call) && !is_retry {
            if let Some(s_id) = session_id {
                crate::proxy::thinking_store::hydrate_gemini_contents_with_model(
                    s_id,
                    contents,
                    Some(target_model),
                );
            }
        }

        // 3. 终审把关与脱敏规范化 (Finalize)
        crate::proxy::thinking_store::finalize_gemini_contents_thinking_with_model(
            contents,
            is_thinking_enabled,
            Some(target_model),
        );
    }

    /// 统一进站思考配置与参数治理（流水线节点）：
    /// 保证四大协议（OpenAI, Claude, Gemini, Codex）的协议无关性。
    /// 1. 自动识别目标模型（包括 Tiered 自适应模型与具名模型）
    /// 2. 忽略客户端数字 budget 防污染，精准捕获客户端无后缀思考参数 (low/medium/high)
    /// 3. 在网关控制模式下，思考参数仅对 Tiered 模型开放操控权，分别映射至网关思考板块的 flash_low, flash_medium, flash_high
    /// 4. 组装并规范化 generationConfig 中的 thinkingConfig 与 maxOutputTokens
    pub fn configure_inbound_thinking(
        target_model: &str,
        generation_config: &mut Value,
        client_switch: ClientThinkingSwitch,
        client_effort: Option<&str>,
        client_budget: Option<u64>,
        token: Option<&crate::proxy::token_manager::ProxyToken>,
    ) -> Option<i64> {
        let is_under_v3 = crate::proxy::model_specs::is_gemini_under_v3(target_model);
        if is_under_v3 {
            // Gemini < 3 非思考模型严禁注入 thinkingConfig
            if let Some(obj) = generation_config.as_object_mut() {
                obj.remove("thinkingConfig");
                obj.remove("thinking_config");
            }
            return None;
        }

        let tb_config = crate::proxy::config::get_thinking_budget_config();

        // ════════════════════════════════════════════════════════════════════
        // 模式分流 1: 客户端自填控制模式（Client Direct Control）
        // 遵循最高指令：思考开关 > 思考等级 > 思考预算，缺省默认开，上游自适应
        // ════════════════════════════════════════════════════════════════════
        if tb_config.control_source == crate::proxy::config::ThinkingControlSource::Client {
            match client_switch {
                ClientThinkingSwitch::Disabled => {
                    // 1. 思考开关显式关闭（一票否决）：彻底不带 thinkingConfig，不填预算
                    if let Some(obj) = generation_config.as_object_mut() {
                        obj.remove("thinkingConfig");
                        obj.remove("thinking_config");
                    }
                    return None;
                }
                ClientThinkingSwitch::Enabled | ClientThinkingSwitch::Default => {
                    // 2. 允许思考（显式开 OR 缺省默认开）
                    // 2.1 预算显式 (> 0)：忠实透传预算数字，绝不脑补等级（防止 Google 400 双字段冲突），必须带上 includeThoughts: true
                    if let Some(budget) = client_budget.filter(|&b| b > 0) {
                        generation_config["thinkingConfig"] = json!({
                            "includeThoughts": true,
                            "thinkingBudget": budget
                        });
                        // 确保 maxOutputTokens 大于 thinkingBudget 避免 400
                        let min_overhead = 8192;
                        let current_max = generation_config
                            .get("maxOutputTokens")
                            .and_then(Value::as_i64)
                            .unwrap_or(65536);
                        if current_max <= budget as i64 {
                            generation_config["maxOutputTokens"] =
                                json!(budget as i64 + min_overhead);
                        }
                        return Some(budget as i64);
                    }

                    // 2.2 预算缺省，但客户端携带了思考等级（包括 low / medium / high 以及任何客户自定义的思考等级）：
                    // 核心铁律：坚决不填预算！忠实透传等级，并且必须带上 includeThoughts: true 核心开关！
                    if let Some(raw_effort) = client_effort.map(str::trim).filter(|s| !s.is_empty())
                    {
                        let lower_effort = raw_effort.to_lowercase();
                        if lower_effort != "default"
                            && lower_effort != "none"
                            && lower_effort != "off"
                            && lower_effort != "disabled"
                        {
                            let final_level = match lower_effort.as_str() {
                                "low" | "extra-low" | "min" | "minimal" => "LOW".to_string(),
                                "medium" | "normal" | "standard" => {
                                    if target_model.to_lowercase().contains("pro") {
                                        "HIGH".to_string()
                                    } else {
                                        "MEDIUM".to_string()
                                    }
                                }
                                "high" | "xhigh" | "max" | "extreme" => "HIGH".to_string(),
                                // 客户带了任何自定义等级，直接忠实透传，绝不硬编码限制！
                                _ => raw_effort.to_uppercase(),
                            };
                            generation_config["thinkingConfig"] = json!({
                                "includeThoughts": true,
                                "thinkingLevel": final_level
                            });
                            return None;
                        }
                    }

                    // 2.3 等级与预算均缺省（或 default）：全部预算不传递，默认上游处理（上游自适应）
                    // ★ 绝对不塞 4000/Medium 预算，仅带 includeThoughts: true
                    generation_config["thinkingConfig"] = json!({
                        "includeThoughts": true
                    });
                    return None;
                }
            }
        }

        // ════════════════════════════════════════════════════════════════════
        // 模式分流 2: 网关权威控制模式（Gateway Authority，99% 用户）
        // 100% 保持原有权威逻辑不变：档位锁死、flash_low/med/high 映射、防 429 注入
        // ════════════════════════════════════════════════════════════════════
        let resolved_budget = crate::proxy::model_specs::resolve_custom_budget(
            target_model,
            client_effort,
            client_budget,
            &tb_config,
            token,
        );

        let is_tiered = crate::proxy::model_specs::is_tiered_flash_model(target_model)
            || target_model.to_lowercase().contains("tiered");

        let mut tc = json!({
            "includeThoughts": true
        });

        if let Some(budget) = resolved_budget {
            if budget == 0 {
                tc = json!({
                    "thinkingBudget": 0
                });
            } else {
                tc["thinkingBudget"] = json!(budget);

                // 确保 maxOutputTokens 大于 thinkingBudget 避免 400
                let min_overhead = 8192;
                let current_max = generation_config
                    .get("maxOutputTokens")
                    .and_then(Value::as_i64)
                    .unwrap_or(65536);
                if current_max <= budget {
                    generation_config["maxOutputTokens"] = json!(budget + min_overhead);
                }
            }
        } else if is_tiered {
            // Tiered 模型未指定具体数字 budget 时，纯自适应模式：不注入 thinkingBudget
            tc = json!({
                "includeThoughts": true
            });
        }

        generation_config["thinkingConfig"] = tc;

        // 终审上限保护
        let target_lower = target_model.to_lowercase();
        let safe_limit = if target_lower.contains("claude") {
            64000
        } else if target_lower.contains("pro") {
            65535
        } else {
            65536
        };
        if let Some(val) = generation_config["maxOutputTokens"].as_i64() {
            if val > safe_limit {
                generation_config["maxOutputTokens"] = json!(safe_limit);
            }
        }

        resolved_budget
    }

    /// 统一规范化与对齐四大协议转译后的 Google Request 前缀拓扑（Pipeline First 核心归一节点）
    /// 确保 OpenAI Chat, OpenAI Responses, Claude 与 Gemini 在上游呈现 100% 字节级同构的前缀：
    /// 1. systemInstruction: 统一规整内部 role -> parts 键序
    /// 2. tools: 清理 Schema，递归转大写 type，绝不拦截任何客户端工具，统一按 name 字母序稳定排序
    /// 3. toolConfig & tool_config: 存在工具时统一补齐并规范模式为 VALIDATED，并开启 includeServerSideToolInvocations
    /// 4. generationConfig: 稳定键序与标准 topK/topP
    /// 5. safetySettings: 缺省统一补齐 4 项 OFF 安全等级，彻底避免跨协议缺失漂移
    /// 6. sessionId: 会话标识
    /// 7. contents: 动态上下文历史
    /// 8. 严格前缀顺序重组: systemInstruction -> tools -> toolConfig -> tool_config -> generationConfig -> safetySettings -> sessionId -> contents
    pub fn align_google_request_prefix_topology(inner_request: &mut Value) {
        if !inner_request.is_object() {
            return;
        }

        // 1. systemInstruction (规范化统一键序: role -> parts -> 其余)
        let canonical_si = if let Some(si) = inner_request.get("systemInstruction") {
            if let Some(si_obj) = si.as_object() {
                let mut c = json!({});
                c["role"] = si_obj.get("role").cloned().unwrap_or(json!("user"));
                if let Some(parts) = si_obj.get("parts") {
                    c["parts"] = parts.clone();
                }
                for (k, v) in si_obj {
                    if k != "role" && k != "parts" {
                        c[k] = v.clone();
                    }
                }
                Some(c)
            } else {
                Some(si.clone())
            }
        } else {
            None
        };

        // 2. tools: 规范化 parameters 并按 name 严格字典序排序，杜绝任何工具拦截过滤
        let canonical_tools = if let Some(tools) = inner_request.get_mut("tools") {
            if let Some(tools_arr) = tools.as_array_mut() {
                for tool in tools_arr.iter_mut() {
                    let decls_opt = if tool.get("functionDeclarations").is_some() {
                        tool.get_mut("functionDeclarations")
                    } else {
                        tool.get_mut("function_declarations")
                    };
                    if let Some(decls) = decls_opt {
                        if let Some(decls_arr) = decls.as_array_mut() {
                            for decl in decls_arr.iter_mut() {
                                if let Some(decl_obj) = decl.as_object_mut() {
                                    if let Some(params_json_schema) =
                                        decl_obj.remove("parametersJsonSchema")
                                    {
                                        let mut params = params_json_schema;
                                        crate::proxy::common::json_schema::clean_json_schema(
                                            &mut params,
                                        );
                                        crate::proxy::mappers::openai::request::enforce_uppercase_types(
                                            &mut params,
                                        );
                                        decl_obj.insert("parameters".to_string(), params);
                                    } else if let Some(params) = decl_obj.get_mut("parameters") {
                                        crate::proxy::common::json_schema::clean_json_schema(
                                            params,
                                        );
                                        crate::proxy::mappers::openai::request::enforce_uppercase_types(
                                            params,
                                        );
                                    }
                                }
                            }
                            decls_arr.sort_by(|a, b| {
                                let name_a = a.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                let name_b = b.get("name").and_then(|v| v.as_str()).unwrap_or("");
                                name_a.cmp(name_b)
                            });
                        }
                    }
                }
            }
            Some(tools.clone())
        } else {
            None
        };

        // 3. toolConfig 与 tool_config (存在工具时双重补齐对齐，模式统一为 VALIDATED)
        let has_tools = canonical_tools
            .as_ref()
            .map_or(false, |t| t.as_array().map_or(false, |arr| !arr.is_empty()));
        let (canonical_tool_config, canonical_tool_config_snake) = if has_tools {
            let mut mode = "VALIDATED";
            if let Some(tc) = inner_request
                .get("toolConfig")
                .or_else(|| inner_request.get("tool_config"))
            {
                let m = tc
                    .get("functionCallingConfig")
                    .or_else(|| tc.get("function_calling_config"))
                    .and_then(|f| f.get("mode"))
                    .and_then(|v| v.as_str());
                if let Some(m_str) = m {
                    if m_str == "NONE" || m_str == "ANY" {
                        mode = m_str;
                    }
                }
            }
            (
                Some(json!({
                    "functionCallingConfig": { "mode": mode },
                    "includeServerSideToolInvocations": true
                })),
                Some(json!({
                    "function_calling_config": { "mode": mode },
                    "include_server_side_tool_invocations": true
                })),
            )
        } else {
            (None, None)
        };

        // 4. generationConfig (对齐默认 topK/topP)
        let canonical_gc = inner_request.get_mut("generationConfig").map(|gc| {
            if let Some(gc_obj) = gc.as_object_mut() {
                if !gc_obj.contains_key("topK") {
                    gc_obj.insert("topK".to_string(), json!(40));
                }
                if !gc_obj.contains_key("topP") {
                    gc_obj.insert("topP".to_string(), json!(1.0));
                }
            }
            gc.clone()
        });

        // 5. safetySettings (统一补齐 4 项 OFF 安全等级，彻底避免跨协议缺失漂移)
        let canonical_safety = if let Some(ss) = inner_request
            .get("safetySettings")
            .and_then(|v| v.as_array())
            .filter(|a| !a.is_empty())
        {
            Some(Value::Array(ss.clone()))
        } else {
            Some(json!([
                { "category": "HARM_CATEGORY_HARASSMENT", "threshold": "OFF" },
                { "category": "HARM_CATEGORY_HATE_SPEECH", "threshold": "OFF" },
                { "category": "HARM_CATEGORY_SEXUALLY_EXPLICIT", "threshold": "OFF" },
                { "category": "HARM_CATEGORY_DANGEROUS_CONTENT", "threshold": "OFF" },
            ]))
        };

        // 6. sessionId
        let canonical_sid = inner_request.get("sessionId").cloned();

        // 7. contents
        let canonical_contents = inner_request.get("contents").cloned().unwrap_or(json!([]));

        // 8. 严格前缀顺序重组:
        // systemInstruction -> tools -> toolConfig -> tool_config -> generationConfig -> safetySettings -> sessionId -> contents
        let mut reordered = json!({});
        if let Some(si) = canonical_si {
            reordered["systemInstruction"] = si;
        }
        if let Some(tools) = canonical_tools {
            reordered["tools"] = tools;
        }
        if let Some(tc) = canonical_tool_config {
            reordered["toolConfig"] = tc;
        }
        if let Some(tc_snake) = canonical_tool_config_snake {
            reordered["tool_config"] = tc_snake;
        }
        if let Some(gc) = canonical_gc {
            reordered["generationConfig"] = gc;
        }
        if let Some(ss) = canonical_safety {
            reordered["safetySettings"] = ss;
        }
        if let Some(sid) = canonical_sid {
            reordered["sessionId"] = sid;
        }
        reordered["contents"] = canonical_contents;

        // 保留其余未知/特定扩展字段在末尾
        if let Some(obj) = inner_request.as_object() {
            for (k, v) in obj {
                if !reordered.as_object().map_or(false, |o| o.contains_key(k)) {
                    reordered[k] = v.clone();
                }
            }
        }
        *inner_request = reordered;
    }

    /// 剥离遗留思考块的前缀标记 (**Thinking**)
    fn strip_thinking_prefix(text: &str) -> String {
        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("**Thinking**") {
            let rest = rest.trim_start_matches(':');
            rest.trim_start_matches(|c| c == '\r' || c == '\n' || c == ' ' || c == '\t')
                .to_string()
        } else {
            text.to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_preserves_process_commentary_alongside_tool_call() {
        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "text": "正在检查网关与后端的连接配置。" },
                {
                    "functionCall": {
                        "name": "inspect_case",
                        "args": { "case": "case_1" }
                    }
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::OpenAIResponses,
            "gemini-2.5-pro",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        // 开启思考时，补齐首位思考块，随后的普通进度文本与工具调用均完整保留
        assert!(parts[0]
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false));
        assert_eq!(parts[1]["text"], "正在检查网关与后端的连接配置。");
        assert!(parts[2].get("functionCall").is_some());
    }

    #[test]
    fn test_preserves_multiple_plain_text_parts_intact() {
        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "text": "第一阶段：检查概览。" },
                { "text": "第二阶段：深入诊断。" },
                {
                    "functionCall": {
                        "name": "run_check",
                        "args": {}
                    }
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::OpenAIChat,
            "gemini-2.5-pro",
            false,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["text"], "第一阶段：检查概览。");
        assert_eq!(parts[1]["text"], "第二阶段：深入诊断。");
        assert!(parts[2].get("functionCall").is_some());
    }

    #[test]
    fn test_heals_legacy_thinking_prefix_without_corrupting_prose() {
        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "text": "**Thinking**\n\n分析了案例数据，准备调用工具。" },
                { "text": "正在执行检查。" },
                {
                    "functionCall": {
                        "name": "inspect",
                        "args": {}
                    }
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::OpenAIResponses,
            "gemini-2.5-pro",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert!(parts[0]
            .get("thought")
            .and_then(Value::as_bool)
            .unwrap_or(false));
        assert_eq!(parts[0]["text"], "分析了案例数据，准备调用工具。");
        assert_eq!(parts[1]["text"], "正在执行检查。");
        assert!(parts[2].get("functionCall").is_some());
    }

    #[test]
    fn test_claude_model_packages_signature_for_google_vertex() {
        let raw_claude_sig = "Eu8CCpIBCBIQAhgCKkAtARbmpPNxYxc/Yz+mpbWJOqMo9c9RF4ESxACD0e/d6SZTpwmbrf9gPP/XMGZ9+kBkTMBfdK7ICuVonHJuu1AcMg9jbGF1ZGUtb3B1cy00LTY4AEIIdGhpbmtpbmdaDDg4NDM1NDkxOTA1MnIQLmWKBlED8AVhXRwj5Lb+PogBAagBosG91QawAQISDFXhwclEQyYNjsDteRoMuuu1Y/dUbn7sPe5OIjBoGvxrSlIgU78kwl701wfF0Rj0BCaCpE6a+KRGaB5pO2vL3ox4+yqum5a8o7mQ8+kqiQFDBTvaDITieiRVrkA8EKBUrpV0rLDyEcL7iQnAMsdQOk31ZKDeBddhEVX+Tb7Qs9mNWXNW9cbrs82iea09O+j2IMs0ibbWXPHB20IlkhVc5q9MmKBYgeQSTzKz+8Tgf7EDd78lkYieVk6GHqQaNiWD1Sl+RO0mIDGwURmOON6Fyw6WkCh/WSF+ORgB";
        let expected_google_vertex_sig = "RXU4Q0NwSUJDQklRQWhnQ0trQXRBUmJtcFBOeFl4Yy9ZeittcGJXSk9xTW85YzlSRjRFU3hBQ0QwZS9kNlNaVHB3bWJyZjlnUFAvWE1HWjkra0JrVE1CZmRLN0lDdVZvbkhKdXUxQWNNZzlqYkdGMVpHVXRiM0IxY3kwMExUWTRBRUlJZEdocGJtdHBibWRhRERnNE5ETTFORGt4T1RBMU1uSVFMbVdLQmxFRDhBVmhYUndqNUxiK1BvZ0JBYWdCb3NHOTFRYXdBUUlTREZYaHdjbEVReVlOanNEdGVSb011dXUxWS9kVWJuN3NQZTVPSWpCb0d2eHJTbElnVTc4a3dsNzAxd2ZGMFJqMEJDYUNwRTZhK0tSR2FCNXBPMnZMM294NCt5cXVtNWE4bzdtUTgra3FpUUZEQlR2YURJVGllaVJWcmtBOEVLQlVycFYwckxEeUVjTDdpUW5BTXNkUU9rMzFaS0RlQmRkaEVWWCtUYjdRczltTldYTlc5Y2JyczgyaWVhMDlPK2oySU1zMGliYldYUEhCMjBJbGtoVmM1cTlNbUtCWWdlUVNUekt6KzhUZ2Y3RURkNzhsa1lpZVZrNkdIcVFhTmlXRDFTbCtSTzBtSURHd1VSbU9PTjZGeXc2V2tDaC9XU0YrT1JnQg==";

        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                {
                    "text": "Let me think about this.",
                    "thought": true,
                    "thoughtSignature": raw_claude_sig
                },
                {
                    "text": "Here is the response."
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::AnthropicClaude,
            "claude-opus-4-6-thinking",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[0]["thoughtSignature"], expected_google_vertex_sig);
        assert_eq!(parts[1]["text"], "Here is the response.");
    }

    #[test]
    fn test_claude_model_from_openai_protocol_packages_signature() {
        let raw_claude_sig = "Eu8CCpIBCBIQAhgCKkAtARbmpPNxYxc/Yz+mpbWJOqMo9c9RF4ESxACD0e/d6SZTpwmbrf9gPP/XMGZ9+kBkTMBfdK7ICuVonHJuu1AcMg9jbGF1ZGUtb3B1cy00LTY4AEIIdGhpbmtpbmdaDDg4NDM1NDkxOTA1MnIQLmWKBlED8AVhXRwj5Lb+PogBAagBosG91QawAQISDFXhwclEQyYNjsDteRoMuuu1Y/dUbn7sPe5OIjBoGvxrSlIgU78kwl701wfF0Rj0BCaCpE6a+KRGaB5pO2vL3ox4+yqum5a8o7mQ8+kqiQFDBTvaDITieiRVrkA8EKBUrpV0rLDyEcL7iQnAMsdQOk31ZKDeBddhEVX+Tb7Qs9mNWXNW9cbrs82iea09O+j2IMs0ibbWXPHB20IlkhVc5q9MmKBYgeQSTzKz+8Tgf7EDd78lkYieVk6GHqQaNiWD1Sl+RO0mIDGwURmOON6Fyw6WkCh/WSF+ORgB";
        let expected_google_vertex_sig = "RXU4Q0NwSUJDQklRQWhnQ0trQXRBUmJtcFBOeFl4Yy9ZeittcGJXSk9xTW85YzlSRjRFU3hBQ0QwZS9kNlNaVHB3bWJyZjlnUFAvWE1HWjkra0JrVE1CZmRLN0lDdVZvbkhKdXUxQWNNZzlqYkdGMVpHVXRiM0IxY3kwMExUWTRBRUlJZEdocGJtdHBibWRhRERnNE5ETTFORGt4T1RBMU1uSVFMbVdLQmxFRDhBVmhYUndqNUxiK1BvZ0JBYWdCb3NHOTFRYXdBUUlTREZYaHdjbEVReVlOanNEdGVSb011dXUxWS9kVWJuN3NQZTVPSWpCb0d2eHJTbElnVTc4a3dsNzAxd2ZGMFJqMEJDYUNwRTZhK0tSR2FCNXBPMnZMM294NCt5cXVtNWE4bzdtUTgra3FpUUZEQlR2YURJVGllaVJWcmtBOEVLQlVycFYwckxEeUVjTDdpUW5BTXNkUU9rMzFaS0RlQmRkaEVWWCtUYjdRczltTldYTlc5Y2JyczgyaWVhMDlPK2oySU1zMGliYldYUEhCMjBJbGtoVmM1cTlNbUtCWWdlUVNUekt6KzhUZ2Y3RURkNzhsa1lpZVZrNkdIcVFhTmlXRDFTbCtSTzBtSURHd1VSbU9PTjZGeXc2V2tDaC9XU0YrT1JnQg==";

        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                {
                    "text": "Thinking process",
                    "thought": true,
                    "thoughtSignature": raw_claude_sig
                },
                {
                    "text": "Answer from OpenAI gateway"
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::OpenAIResponses,
            "claude-sonnet-4-6",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thoughtSignature"], expected_google_vertex_sig);
        assert_eq!(parts[1]["text"], "Answer from OpenAI gateway");
    }

    #[test]
    fn test_gemini_native_signature_preserved_without_double_encoding() {
        let gemini_sig = "EudDCuRDAWkUfRO9pMXsHitwdfey4TDAgCv1WzzMfBXVamvaqJ01BJPawr58";

        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                {
                    "text": "Gemini thinking",
                    "thought": true,
                },
                {
                    "thoughtSignature": gemini_sig,
                    "functionCall": {
                        "name": "bash",
                        "args": { "command": "ls" }
                    }
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::GeminiNative,
            "gemini-3.8-flash-high",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        // Gemini 原生签名在工具调用轮次绝不被二次编码，必须原样保留在 functionCall 部件上
        assert_eq!(parts[1]["thoughtSignature"], gemini_sig);
    }

    #[test]
    fn test_inbound_pipeline_intercepts_foreign_claude_signature_for_gemini() {
        let foreign_claude_sig = "3mgp11XmVXq9InniGA4VAKd7c97NqFw+dWZt79Uz/w9znho88gSM76jv2bZmir7wI86Ixpha7eWdGuznAot4PNbe3+V9bgMTIEyUarn4MLAiiFVb830ZlM+H5ukQwXdD2Zv8nUSmmZTYinpLPGha8TORZAfpU1FJEvwyECel5+W7kc9kpTWrd8DqRNBTOz5EDtvoatiZgKv5SqInhGXK74SJ+PRIC6fNXvYG082HR6TsVxvVYaerz8A40rloIVTxRNK43h3Ecs1boxY4PZqBT8Yhl2qn/iZ+4Xt7FNkI0DAuS9iK0HYKMC4yw0OqKx/LeU+WFZlyc6hGm1BkzLY6yG97MH7kmJ0OPlBWgWFaTeL/uXuGJX6QkKObXN+phoq+kkF2vdFt/mdJMbdgfmSCVQ9037hGBhOHm0zN50KLkp1SxuAY1oWc+lDcI4ufWoyn";

        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                {
                    "text": "Cross-model thinking from Claude",
                    "thought": true,
                    "thoughtSignature": foreign_claude_sig
                },
                {
                    "thoughtSignature": foreign_claude_sig,
                    "functionCall": {
                        "name": "bash",
                        "args": { "command": "ls" }
                    }
                }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::AnthropicClaude,
            "gemini-3.7-flash-high",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0]["thought"], true);
        assert!(
            parts[0].get("thoughtSignature").is_none(),
            "Thinking block for Gemini should NOT carry foreign signature or sentinel in pure text"
        );
        assert_eq!(
            parts[1]["thoughtSignature"],
            crate::proxy::thinking_store::SENTINEL_SIGNATURE,
            "FunctionCall must fall back to sentinel signature in InboundThinkingPipeline"
        );
    }

    #[test]
    fn test_inbound_pipeline_intercepts_foreign_gemini_signature_for_claude() {
        // 模拟 Gemini 原生签名
        let foreign_gemini_sig =
            "Ep4KCpsKAWkUfRMa5ZYMDdlPjxrQTLzVZ6MZeopI88888888888888888888888888888888";

        let mut contents = vec![
            json!({
                "role": "user",
                "parts": [{ "text": "hello" }]
            }),
            json!({
                "role": "model",
                "parts": [
                    {
                        "text": "The input is a Chinese greeting...",
                        "thought": true,
                        "thoughtSignature": foreign_gemini_sig
                    },
                    {
                        "text": "Hello! How can I help you today?"
                    }
                ]
            }),
            json!({
                "role": "user",
                "parts": [{ "text": "continue" }]
            }),
        ];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::OpenAIChat,
            "claude-opus-4-6-thinking",
            true,
            None,
            false,
        );

        let model_parts = contents[1]["parts"].as_array().expect("parts array");
        // 关键验证：发往 Claude 时，由于历史异构签名不是合法 Claude 签名，
        // 思考块绝不能带着 Gemini 签名发给 Claude，而是安全降级为普通正文文本！
        let has_thought_block = model_parts
            .iter()
            .any(|p| p.get("thought").and_then(|v| v.as_bool()) == Some(true));
        assert!(
            !has_thought_block,
            "Claude turn must NOT contain unvalidated thinking block with foreign Gemini signature"
        );
        let has_gemini_sig = model_parts
            .iter()
            .any(|p| p.get("thoughtSignature").is_some() || p.get("thought_signature").is_some());
        assert!(
            !has_gemini_sig,
            "Foreign Gemini signature must be completely eliminated from Claude turn"
        );
    }

    #[test]
    fn test_inbound_pipeline_lifts_multimodal_images_from_function_response() {
        let fake_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
        let mut contents = vec![json!({
            "role": "user",
            "parts": [{
                "functionResponse": {
                    "name": "take_screenshot",
                    "response": {
                        "output": format!("Screenshot result: ![view](data:image/png;base64,{}) done.", fake_b64)
                    }
                }
            }]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::GeminiNative,
            "gemini-2.5-flash",
            false,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        assert_eq!(
            parts.len(),
            2,
            "Should have functionResponse and lifted inlineData"
        );
        assert!(parts[0].get("functionResponse").is_some());
        assert!(parts[1].get("inlineData").is_some());

        assert_eq!(parts[1]["inlineData"]["mimeType"], "image/png");
        assert_eq!(parts[1]["inlineData"]["data"], fake_b64);

        let output_text = parts[0]["functionResponse"]["response"]["output"]
            .as_str()
            .unwrap();
        assert!(!output_text.contains(fake_b64));
        assert!(output_text.contains("[Image: forwarded to visual input (image/png)]"));
    }

    #[test]
    fn test_extract_client_thinking_switch_coverage() {
        // 1. 显式关闭 (一票否决)
        assert_eq!(
            extract_client_thinking_switch(Some("disabled"), None, None),
            ClientThinkingSwitch::Disabled
        );
        assert_eq!(
            extract_client_thinking_switch(Some("off"), None, None),
            ClientThinkingSwitch::Disabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, Some(0), None),
            ClientThinkingSwitch::Disabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, None, Some("none")),
            ClientThinkingSwitch::Disabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, None, Some("off")),
            ClientThinkingSwitch::Disabled
        );

        // 2. 显式开启
        assert_eq!(
            extract_client_thinking_switch(Some("enabled"), None, None),
            ClientThinkingSwitch::Enabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, Some(1024), None),
            ClientThinkingSwitch::Enabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, None, Some("low")),
            ClientThinkingSwitch::Enabled
        );
        assert_eq!(
            extract_client_thinking_switch(None, None, Some("high")),
            ClientThinkingSwitch::Enabled
        );

        // 3. 缺省（开关缺省就是默认开）
        assert_eq!(
            extract_client_thinking_switch(None, None, None),
            ClientThinkingSwitch::Default
        );
        assert_eq!(
            extract_client_thinking_switch(Some("default"), None, None),
            ClientThinkingSwitch::Default
        );
        assert_eq!(
            extract_client_thinking_switch(None, None, Some("default")),
            ClientThinkingSwitch::Default
        );
    }

    #[test]
    fn test_configure_inbound_thinking_client_mode_routing_clean_isolation() {
        use crate::proxy::config::{
            update_thinking_budget_config, ThinkingBudgetConfig, ThinkingControlSource,
        };

        let mut config = ThinkingBudgetConfig::default();
        config.control_source = ThinkingControlSource::Client;
        update_thinking_budget_config(config);

        struct ResetGuard;
        impl Drop for ResetGuard {
            fn drop(&mut self) {
                crate::proxy::config::update_thinking_budget_config(ThinkingBudgetConfig::default());
            }
        }
        let _guard = ResetGuard;

        // 1. 显式关闭：彻底不带 thinkingConfig
        let mut gc1 = json!({
            "thinkingConfig": { "includeThoughts": true }
        });
        InboundThinkingPipeline::configure_inbound_thinking(
            "gemini-3.8-flash-tiered",
            &mut gc1,
            ClientThinkingSwitch::Disabled,
            None,
            None,
            None,
        );
        assert!(gc1.get("thinkingConfig").is_none());

        // 2. 缺省：includeThoughts=true，绝无 thinkingBudget
        let mut gc2 = json!({});
        InboundThinkingPipeline::configure_inbound_thinking(
            "gemini-3.8-flash-tiered",
            &mut gc2,
            ClientThinkingSwitch::Default,
            None,
            None,
            None,
        );
        let tc2 = gc2.get("thinkingConfig").unwrap().as_object().unwrap();
        assert_eq!(tc2.get("includeThoughts"), Some(&json!(true)));
        assert!(tc2.get("thinkingBudget").is_none());
        assert!(tc2.get("thinkingLevel").is_none());

        // 3. 显式等级：thinkingLevel=LOW / HIGH，绝无 thinkingBudget，必须带上 includeThoughts: true
        let mut gc3 = json!({});
        InboundThinkingPipeline::configure_inbound_thinking(
            "gemini-3.8-flash-tiered",
            &mut gc3,
            ClientThinkingSwitch::Enabled,
            Some("low"),
            None,
            None,
        );
        let tc3 = gc3.get("thinkingConfig").unwrap().as_object().unwrap();
        assert_eq!(tc3.get("includeThoughts"), Some(&json!(true)));
        assert_eq!(tc3.get("thinkingLevel"), Some(&json!("LOW")));
        assert!(tc3.get("thinkingBudget").is_none());

        // 3.1 客户端填了任何自定义等级（非硬编码）且没填预算，忠实透传等级，坚决不填预算
        let mut gc3_custom = json!({});
        let budget_custom_res = InboundThinkingPipeline::configure_inbound_thinking(
            "gemini-3.8-flash-tiered",
            &mut gc3_custom,
            ClientThinkingSwitch::Enabled,
            Some("custom_ultra_level"),
            None,
            None,
        );
        let tc3_custom = gc3_custom
            .get("thinkingConfig")
            .unwrap()
            .as_object()
            .unwrap();
        assert_eq!(tc3_custom.get("includeThoughts"), Some(&json!(true)));
        assert_eq!(
            tc3_custom.get("thinkingLevel"),
            Some(&json!("CUSTOM_ULTRA_LEVEL"))
        );
        assert!(tc3_custom.get("thinkingBudget").is_none(), "When client provides custom effort without budget, thinkingBudget must strictly remain None");
        assert!(budget_custom_res.is_none());

        // 4. 显式预算：thinkingBudget=8192，绝无 thinkingLevel，必须带上 includeThoughts: true
        let mut gc4 = json!({});
        InboundThinkingPipeline::configure_inbound_thinking(
            "gemini-3.8-flash-tiered",
            &mut gc4,
            ClientThinkingSwitch::Enabled,
            None,
            Some(8192),
            None,
        );
        let tc4 = gc4.get("thinkingConfig").unwrap().as_object().unwrap();
        assert_eq!(tc4.get("includeThoughts"), Some(&json!(true)));
        assert_eq!(tc4.get("thinkingBudget"), Some(&json!(8192)));
        assert!(tc4.get("thinkingLevel").is_none());
    }

    #[test]
    fn test_cross_family_think_tag_extraction_and_elevation_for_gemini() {
        let thought_text = "Analyzing user code structure and determining route.";
        let visible_answer = "The issue has been identified and isolated.";
        let wrapped_text = format!("<think>\n{}\n</think>\n\n{}", thought_text, visible_answer);

        let mut contents = vec![json!({
            "role": "model",
            "parts": [
                { "text": wrapped_text }
            ]
        })];

        InboundThinkingPipeline::process_contents(
            &mut contents,
            ProxyProtocol::AnthropicClaude,
            "gemini-3.8-flash-tiered",
            true,
            None,
            false,
        );

        let parts = contents[0]["parts"].as_array().expect("parts array");
        // 1. 首位成功提升为 thought: true 的思考块
        assert_eq!(parts[0]["thought"], true);
        assert_eq!(parts[0]["text"], thought_text);
        assert_eq!(
            parts[0]["thoughtSignature"],
            crate::proxy::thinking_store::SENTINEL_SIGNATURE
        );

        // 2. 正文部件已干净剔除 <think>...</think> 标签与换行，仅保留真实回答
        assert_eq!(parts[1]["text"], visible_answer);
        assert!(parts[1].get("thought").is_none());
    }
}
