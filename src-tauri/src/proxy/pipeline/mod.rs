//! 全双工模型思考与响应统一流水线架构
//!
//! 1. 统一中间领域对象（Canonical IR）：谷歌 Gemini 标准报文 (`contents` + `generationConfig`)
//! 2. 协议进站策略适配器 (`InboundThinkingPipeline`, `ProxyProtocol`, `UpstreamClassification`)
//! 3. 统一用量与缓存核心计算收拢与散开 (`CanonicalUsage`)
//!
//! 架构设计规范：出站发散由各协议 mapper 适配器独立实现（Gemini -> 各协议线缆格式）。
//! 出站不设统一流水线，保持协议发散的灵活性与流式稳定性。

pub mod inbound;
pub mod policy;
pub mod usage;

pub use inbound::{extract_client_thinking_switch, ClientThinkingSwitch, InboundThinkingPipeline};
pub use policy::{ProxyProtocol, UpstreamClassification};
pub use usage::CanonicalUsage;
