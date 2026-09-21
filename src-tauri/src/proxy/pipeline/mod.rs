//! 全双工模型思考与响应统一流水线架构
//!
//! 1. 统一中间领域对象（Canonical IR）：谷歌 Gemini 标准报文 (`contents` + `generationConfig`)
//! 2. 协议进站策略适配器 (`InboundThinkingPipeline`, `ProxyProtocol`)
//! 3. 统一用量与缓存核心计算收拢与散开 (`CanonicalUsage`)
//! 4. 流式与非流式统一出站萃取、反向入库与协议散开 (`OutboundThinkingPipeline`, `CanonicalStreamEvent`)

pub mod canonical;
pub mod events;
pub mod inbound;
pub mod outbound;
pub mod policy;
pub mod usage;

pub use events::CanonicalStreamEvent;
pub use inbound::InboundThinkingPipeline;
pub use outbound::{CanonicalEgressPayload, OutboundThinkingPipeline};
pub use policy::{ProxyProtocol, UpstreamClassification};
pub use usage::CanonicalUsage;
