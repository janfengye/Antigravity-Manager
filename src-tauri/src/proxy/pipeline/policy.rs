use serde::{Deserialize, Serialize};

/// 代理所接入与服务的客户端协议类型
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ProxyProtocol {
    OpenAIChat,
    OpenAIResponses,
    AnthropicClaude,
    GeminiNative,
}

impl ProxyProtocol {
    /// 该协议是否信任客户端回传的思考签名
    /// - OpenAIChat: false (官方规范无签名概念，客户端若夹带也属于不可信，入站统一擦除，由服务端全权参与回填)
    /// - OpenAIResponses / AnthropicClaude / GeminiNative: true (协议原生支持签名，校验长度与兼容性后采纳)
    pub fn trusts_client_signature(&self) -> bool {
        match self {
            ProxyProtocol::OpenAIChat => false,
            ProxyProtocol::OpenAIResponses
            | ProxyProtocol::AnthropicClaude
            | ProxyProtocol::GeminiNative => true,
        }
    }

    /// 出站时是否向客户端回显/透传思考签名
    /// - OpenAIChat: false (OpenAI Chat API 仅接受 reasoning_content，绝不输出签名)
    /// - OpenAIResponses / AnthropicClaude / GeminiNative: true (输出原生签名或加密字段)
    pub fn emits_signature_to_client(&self) -> bool {
        match self {
            ProxyProtocol::OpenAIChat => false,
            ProxyProtocol::OpenAIResponses
            | ProxyProtocol::AnthropicClaude
            | ProxyProtocol::GeminiNative => true,
        }
    }

    /// 协议显示名称
    pub fn display_name(&self) -> &'static str {
        match self {
            ProxyProtocol::OpenAIChat => "OPENAI_CHAT",
            ProxyProtocol::OpenAIResponses => "OPENAI_RESPONSES",
            ProxyProtocol::AnthropicClaude => "ANTHROPIC",
            ProxyProtocol::GeminiNative => "GEMINI",
        }
    }
}
