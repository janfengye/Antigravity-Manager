use super::usage::CanonicalUsage;
use serde::{Deserialize, Serialize};

/// 流式增量分块（Streaming SSE Chunks）的统一领域事件抽象
/// 上游 Google Gemini SSE 分块在转为具体协议前，先收拢萃取为本事件
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CanonicalStreamEvent {
    /// 思考思维链增量 (Thought Delta)
    ThoughtDelta(String),
    /// 思考签名 (Google 签发的加密签名)
    ThoughtSignature(String),
    /// 用户可见正文增量 (Text Delta)
    TextDelta(String),
    /// 工具调用增量 (Tool Call Delta)
    ToolCallDelta {
        index: usize,
        id: Option<String>,
        name: Option<String>,
        args_delta: String,
    },
    /// 用量增量更新 (Usage Update)
    UsageUpdate(CanonicalUsage),
    /// 流结束事件 (Finish)
    Finish {
        reason: String,
        usage: Option<CanonicalUsage>,
    },
}
