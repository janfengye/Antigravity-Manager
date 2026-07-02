//! Context Cache Manager
//!
//! 维护 prefix_hash → Gemini cache_name 的映射表，利用 Gemini 显式缓存节省 Token。
//!
//! 架构:
//! - L1: 内存 DashMap (prefix_hash → CacheEntry)，进程内快速查找
//! - 未来可扩展 L2: SQLite 持久化 或 Redis 共享
//!
//! 缓存生命周期:
//! 1. 请求到达 → compute_prefix_hash(systemInstruction + tools) → lookup(hash)
//! 2. 若命中 → 注入 cachedContent 参数到请求体
//! 3. 若未命中 → 正常发送请求，响应后从 usageMetadata 提取缓存信息
//! 4. 后台任务定期淘汰过期条目

use dashmap::DashMap;
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// 缓存条目
#[derive(Debug, Clone)]
struct CacheEntry {
    /// Gemini 返回的缓存资源名
    /// 格式: 由 API 响应中的 cachedContent 字段提供
    /// 若 API 不返回显式缓存名，则用 prefix_hash 作为本地追踪 ID
    cache_name: String,
    /// 创建时间
    created_at: Instant,
    /// 过期时间
    expires_at: Instant,
    /// 隐式缓存命中次数 (从 cachedContentTokenCount > 0 统计)
    implicit_hit_count: u64,
    /// 显式缓存命中次数 (成功使用 cachedContent 参数)
    explicit_hit_count: u64,
}

/// 缓存统计信息
#[derive(Debug, Clone, Default)]
pub struct CacheStats {
    /// 总查找次数
    pub total_lookups: u64,
    /// 缓存命中次数
    pub hits: u64,
    /// 缓存未命中次数
    pub misses: u64,
    /// 当前活跃缓存条目数
    pub active_entries: usize,
    /// 总隐式命中次数
    pub total_implicit_hits: u64,
}

/// Context Cache Manager 单例
pub struct CacheManager {
    /// prefix_hash → CacheEntry
    cache: DashMap<String, CacheEntry>,
    /// 统计信息
    stats: std::sync::RwLock<CacheStats>,
    /// 默认 TTL (秒)
    default_ttl_secs: u64,
}

impl CacheManager {
    /// 创建新的 CacheManager
    pub fn new(default_ttl_secs: u64) -> Self {
        Self {
            cache: DashMap::new(),
            stats: std::sync::RwLock::new(CacheStats::default()),
            default_ttl_secs,
        }
    }

    /// 计算稳定前缀的 SHA256 哈希
    ///
    /// 输入: systemInstruction 和 tools 的 JSON 字符串
    /// 这两部分在多次 Codex 请求中几乎不变，是缓存的核心
    pub fn compute_prefix_hash(system_instruction_json: &str, tools_json: &str) -> String {
        let mut hasher = Sha256::new();
        hasher.update(system_instruction_json.as_bytes());
        if !tools_json.is_empty() {
            hasher.update(b"\n---TOOLS---\n");
            hasher.update(tools_json.as_bytes());
        }
        format!("{:x}", hasher.finalize())
    }

    /// 查找缓存的 cache_name
    ///
    /// 返回 Some(cache_name) 如果存在有效缓存，否则 None
    pub fn lookup(&self, hash: &str) -> Option<String> {
        // 更新统计
        if let Ok(mut stats) = self.stats.write() {
            stats.total_lookups += 1;
        }

        match self.cache.get(hash) {
            Some(entry) if entry.expires_at > Instant::now() => {
                // 缓存有效
                if let Ok(mut stats) = self.stats.write() {
                    stats.hits += 1;
                }
                tracing::debug!(
                    "[CacheManager] Hit: hash={} cache_name={} age={:?}",
                    &hash[..hash.len().min(16)],
                    entry.cache_name,
                    entry.created_at.elapsed()
                );
                Some(entry.cache_name.clone())
            }
            Some(_) => {
                // 缓存已过期，记录 key 然后移除
                let expired_key = hash.to_string();
                drop(self.cache.remove(&expired_key));
                if let Ok(mut stats) = self.stats.write() {
                    stats.misses += 1;
                }
                tracing::debug!(
                    "[CacheManager] Expired: hash={}",
                    &hash[..hash.len().min(16)]
                );
                None
            }
            None => {
                if let Ok(mut stats) = self.stats.write() {
                    stats.misses += 1;
                }
                tracing::debug!(
                    "[CacheManager] Miss: hash={}",
                    &hash[..hash.len().min(16)]
                );
                None
            }
        }
    }

    /// 插入或更新缓存条目
    ///
    /// cache_name: Gemini 返回的缓存资源名。如果 API 不支持显式缓存返回，
    /// 可以使用 prefix_hash 本身作为 fallback ID 用于追踪。
    pub fn insert(&self, hash: String, cache_name: String, ttl_secs: Option<u64>) {
        let ttl = ttl_secs.unwrap_or(self.default_ttl_secs);
        let now = Instant::now();
        let entry = CacheEntry {
            cache_name,
            created_at: now,
            expires_at: now + Duration::from_secs(ttl),
            implicit_hit_count: 0,
            explicit_hit_count: 0,
        };
        tracing::info!(
            "[CacheManager] Insert: hash={} ttl={}s",
            &hash[..hash.len().min(16)],
            ttl
        );
        self.cache.insert(hash, entry);
    }

    /// 记录一次隐式缓存命中（来自响应的 cachedContentTokenCount > 0）
    pub fn record_implicit_hit(&self, hash: &str) {
        if let Some(mut entry) = self.cache.get_mut(hash) {
            entry.implicit_hit_count += 1;
        }
        if let Ok(mut stats) = self.stats.write() {
            stats.total_implicit_hits += 1;
        }
    }

    /// 获取缓存统计
    pub fn get_stats(&self) -> CacheStats {
        let mut stats = self.stats.read().unwrap().clone();
        stats.active_entries = self.cache.len();
        stats
    }

    /// 淘汰过期条目
    pub fn evict_expired(&self) -> usize {
        let now = Instant::now();
        let expired: Vec<String> = self
            .cache
            .iter()
            .filter(|entry| entry.expires_at <= now)
            .map(|entry| entry.cache_name.clone())
            .collect();

        let count = expired.len();
        for key in &expired {
            self.cache.remove(key);
        }

        if count > 0 {
            tracing::debug!("[CacheManager] Evicted {} expired entries", count);
        }
        count
    }

    /// 清空所有缓存
    pub fn clear(&self) {
        let count = self.cache.len();
        self.cache.clear();
        if let Ok(mut stats) = self.stats.write() {
            *stats = CacheStats::default();
        }
        tracing::info!("[CacheManager] Cleared {} entries", count);
    }
}

/// 全局 CacheManager 单例
use std::sync::LazyLock;
static GLOBAL_CACHE_MANAGER: LazyLock<CacheManager> =
    LazyLock::new(|| CacheManager::new(3600)); // 默认 TTL 1 小时

/// 获取全局 CacheManager
pub fn global_cache_manager() -> &'static CacheManager {
    &GLOBAL_CACHE_MANAGER
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    #[test]
    fn test_compute_prefix_hash_deterministic() {
        let si1 = r#"{"role":"user","parts":[{"text":"system prompt"}]}"#;
        let tools1 = r#"[{"functionDeclarations":[{"name":"test_tool"}]}]"#;

        let hash1 = CacheManager::compute_prefix_hash(si1, tools1);
        let hash2 = CacheManager::compute_prefix_hash(si1, tools1);
        assert_eq!(hash1, hash2, "Same inputs must produce same hash");
    }

    #[test]
    fn test_compute_prefix_hash_different() {
        let si1 = r#"{"role":"user","parts":[{"text":"system A"}]}"#;
        let si2 = r#"{"role":"user","parts":[{"text":"system B"}]}"#;
        let tools = r#"[{"functionDeclarations":[]}]"#;

        let hash1 = CacheManager::compute_prefix_hash(si1, tools);
        let hash2 = CacheManager::compute_prefix_hash(si2, tools);
        assert_ne!(hash1, hash2, "Different inputs must produce different hash");
    }

    #[test]
    fn test_lookup_miss() {
        let manager = CacheManager::new(60);
        let result = manager.lookup("nonexistent_hash");
        assert!(result.is_none());
    }

    #[test]
    fn test_insert_and_lookup() {
        let manager = CacheManager::new(60);
        let hash = "test_hash_123".to_string();
        let cache_name = "cachedContents/test123".to_string();

        manager.insert(hash.clone(), cache_name.clone(), None);
        let result = manager.lookup(&hash);
        assert_eq!(result, Some(cache_name));
    }

    #[test]
    fn test_expiry() {
        let manager = CacheManager::new(0); // TTL=0 立即过期
        let hash = "expiring_hash".to_string();
        manager.insert(hash.clone(), "cache_name".to_string(), Some(0));

        // 等待一下确保过期
        thread::sleep(Duration::from_millis(10));
        let result = manager.lookup(&hash);
        assert!(result.is_none(), "Expired entry should not be returned");
    }

    #[test]
    fn test_evict_expired() {
        let manager = CacheManager::new(0);
        for i in 0..5 {
            manager.insert(format!("hash_{}", i), format!("cache_{}", i), Some(0));
        }
        thread::sleep(Duration::from_millis(10));
        let evicted = manager.evict_expired();
        assert_eq!(evicted, 5);
        assert_eq!(manager.cache.len(), 0);
    }

    #[test]
    fn test_stats() {
        let manager = CacheManager::new(60);
        manager.lookup("hash1"); // miss
        manager.insert("hash2".to_string(), "cache2".to_string(), None);
        manager.lookup("hash2"); // hit

        let stats = manager.get_stats();
        assert_eq!(stats.total_lookups, 2);
        assert_eq!(stats.hits, 1);
        assert_eq!(stats.misses, 1);
        assert_eq!(stats.active_entries, 1);
    }

    #[test]
    fn test_global_singleton() {
        let cm = global_cache_manager();
        cm.clear();
        let stats = cm.get_stats();
        assert_eq!(stats.total_lookups, 0);
        assert_eq!(stats.active_entries, 0);
    }
}
