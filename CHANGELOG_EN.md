# 📝 Changelog

> Complete version history for Antigravity Tools. Return to project home at [README_EN.md](README_EN.md).

*   **Version History**:
    *   **v4.6.3 (2026-08-30)**:
        -   **[Core Fix] Account JSON Storage Self-Healing & Concurrent File Write Lock (Issue #3345)**:
            -   **Self-Healing Parser on Load**: Added streaming deserializer fallback when reading account files. If an account file has trailing characters or extra closing braces (e.g. `trailing characters at line ...`), the parser automatically recovers the valid full `Account` data and atomically rewrites a clean file back to disk, completely preventing accounts from silently disappearing from the UI and causing cascading 429 rate limit outages.
            -   **Per-Account Concurrency Write Lock**: Introduced a global per-account mutex lock mechanism (`ACCOUNT_FILE_LOCKS`) to ensure strict serialized thread safety across concurrent quota refreshes, 429 rate-limit event writes, and `last_used` touch operations.
        -   **[Core Fix] Fix Discrete Model Chip Rendering for Pinned Gemini 3.7 Flash Models (Issue #3344)**:
            -   **Exact Match Priority**: Introduced exact-model matching in `resolveQuotaModels`. When a pinned selector matches a real quota model name (such as `gemini-3.7-flash-low`, `gemini-3.7-flash-high`, etc.), it renders as an independent discrete chip (`model:${id}`) instead of being collapsed and deduplicated into a single legacy `category:gemini-flash` slot that was hardcoded to older models.
            -   **Backward Compatibility**: Unmatched legacy category selectors and image selectors continue to use category-based resolution, preserving backward compatibility.
        -   **[Feature Optimization] Dashboard Best Accounts Recommendation with 5h & Weekly Quota Evaluation (Issue #3343)**:
            -   **Dual-Window Bottleneck Constraint**: Evaluates both the 5-hour rolling window and the 7-day weekly quota constraint ($\min(5h, weekly)$), avoiding recommending accounts that have a full 5h quota but have exhausted their weekly allowance.
            -   **Free Tier Single-Bucket Support**: Automatically detects single/dual-bucket account structures. Accounts with only a weekly quota (Free Tier) smoothly use their weekly quota percentage for evaluation, ensuring fair ranking without false zeroing.
            -   **Exhaustion Circuit Breaker**: Accounts with weekly quota $\le 5\%$ are disqualified from recommendation to prevent switching to unusable accounts.
        -   **[Core Fix] Gemini 3.7 / 3.x Thought-Signature Invalidation & Multi-Turn Variant Compatibility (PR #3342)**:
            -   **Case-Insensitive Thought Signature Error Matching**: Used `to_lowercase()` matching in Claude protocol and common handlers to capture all Google thought signature error variants (`Invalid thought signature.`, `thought_signature`, `thoughtsignature`), reliably triggering automatic retry and signature stripping.
            -   **Gemini 3.x Model Compatibility Rules**: Added explicit compatibility rules for `gemini-3.x` (Flash / Pro families) and `gemini-3.7` in `is_model_compatible`, ensuring thought signatures persist correctly across laddered variant turns.
        -   **[i18n] 100% Full Localization Across Multiple Languages (PR #3338, PR #3339, PR #3340, PR #3341)**:
            -   **Japanese (ja.json, PR #3338)**: Complete translations for quota protection, smart warmup, adaptive circuit breaker, context compression, model routing, and Homebrew updater; cleared residual strings.
            -   **Spanish (es.json, PR #3339)**: Complete translations for Proxy Pool, Debug Console, Network Monitor, IP Security Whitelist/Blacklist, OpenCode sync, and APIKEY.FUN relay.
            -   **Russian (ru.json, PR #3340)**: Complete translations for HTTP API server settings, Debug console, Homebrew upgrade workflow, Context Compression (Caveman/L1-L3), and streaming error prompts.
            -   **Korean (ko.json, PR #3341)**: Complete translations for proxy pool, debug console, model routing presets, 403 quick fix guide, and Homebrew update notifications.
    *   **v4.6.2 (2026-08-28)**:
        -   **[Core Fix] Proxy Startup Diagnostics for Silent Failure & Unreachable Ports (PR #3330)**:
            -   **Startup Failure Logging**: Added explicit `error!` logs when `load_app_config()` fails in `lib.rs`, converting silent exits into actionable error logs and clarifying that services were not started.
            -   **Cleaned Up Dummy Server Handle**: Removed unneeded placeholder `tokio::spawn(async {})` handles from `ProxyServiceInstance`, leaving unified lifecycle management to `AdminServerInstance`.
        -   **[i18n] Brazilian Portuguese (pt-BR) 100% Key Alignment with en.json (PR #3334)**:
            -   **1224+ Translation Keys Completed**: Translated all missing keys and removed residual Chinese strings (0 missing, 0 mismatched).
            -   **Placeholder Synchronization**: Aligned `{{name}}`, `{{error}}` interpolation parameters to avoid runtime UI render issues.
            -   **Component Direct References**: Added missing keys directly referenced by frontend TSX components.
        -   **[Enhancement] Model Catalog Update, Official Icons & OpenCode Sync Optimization (PR #3335)**:
            -   **New Model Support**: Added `gemini-3.7-flash`, `gemini-3.1-flash-lite`, `claude-opus-4-6`, `gpt-oss-120b-medium` with `@lobehub/icons` official brand icons.
            -   **Model List Deduplication**: Normalized alias mappings in `useProxyModels` to eliminate duplicate model entries caused by sub-tier suffixes.
            -   **OpenCode Sync Adjustments**: Enabled `ClaudeThinking` reasoning variants for Claude models and disabled unsupported `max` variants for Gemini 3 series.
        -   **[Platform Fix] Eliminate Windows Background Process Console Flashing (PR #3336)**:
            -   **Unified CREATE_NO_WINDOW Flags**: Replaced/supplemented `DETACHED_PROCESS` with `CREATE_NO_WINDOW` (0x08000000) across Cloudflared, tar decompression, and manual executable calls to eliminate console windows popping up.
            -   **Sync/Async Unified Handling**: Applied no-window flags consistently across `std::process::Command` and `tokio::process::Command` extensions.
        -   **[Core Fix] Fix Gemini 3.x 400 Bad Request on Thinking Block Compression (PR #3337)**:
            -   **Root Cause**: When `ContextManager` compressed thinking content to `"..."`, it preserved the original `thoughtSignature`, causing Google API to fail validation with `400 INVALID_ARGUMENT: Invalid thought signature`.
            -   **Fix**: Cleared the corresponding signature field when compressing thinking content to maintain signature chain integrity.
        -   **[Install Script Fix] Fix Linux Install Script 404 on Version Parsing (Issue #3328)**:
            -   **Validation & Direct Redirection**: Added `_is_valid_version()` semantic version format validation and switched Method 2 to `curl -w '%{url_effective}'` to avoid header parsing whitespace issues.
    *   **v4.6.1 (2026-08-25)**:
        -   **[Core Fix] Prevent 1M Token Overflow on Long Multi-Turn Thinking & Local Token Estimation Fallback (Issue #3325)**:
            -   **Historical Thinking Pruning**: When converting to Gemini contents, only the most recent window of assistant thinking text is preserved; older turns retain only `thoughtSignature` placeholders to prevent context from exceeding the 1M token ceiling.
            -   **Fallback Token Estimation**: If upstream Google returns an error without `usageMetadata`, middleware uses the local token estimation engine to calculate `input_tokens`, preventing blank token stats in monitor logs.
        -   **[Core Fix] JSON Schema `const` Keyword Normalization for Computer Use MCP (Issue #3327)**:
            -   **Schema Sanitization**: Automatically converts `{"const": "value"}` into standard `{"type": "...", "enum": ["value"]}` compatible with Gemini/Vertex Schema Proto.
            -   **Nested & Union Types Support**: Full support for `anyOf`/`oneOf` unions and deeply nested objects containing `const` fields.
    *   **v4.6.0 (2026-08-24)**:
        -   **[Core Feature] OpenAI Endpoint Supports `response_format.json_schema` Structured Outputs (PR #3324)**:
            -   **JSON Schema Support**: Full support for `response_format: { type: "json_schema", json_schema: { ... } }`.
            -   **Recursive Schema Unfolding**: Automatically extracts and sanitizes `$ref`/`$defs` definitions, converting schemas into Gemini `generationConfig.responseSchema` standards with `responseMimeType: "application/json"`.
        -   **[Core Fix] Proxy Pool Health Check 407 & URL Inline Auth Parsing Fix (Issue #3323)**:
            -   **HTTPS 204 Health Check**: Upgraded default health check endpoint to `https://cp.cloudflare.com/generate_204` via standard HTTPS `CONNECT` tunnels, eliminating false `407 Proxy Authentication Required` errors.
            -   **Inline Credentials Parsing**: Safely extracts `username` and `password` from `http(s)://user:pass@ip:port` proxy URLs and injects HTTP Basic Auth.
        -   **[Core Fix] Gemini 3.7 / 3.6 Flash Variant Mapping & 429 Fix (Issue #3322)**:
            -   **Registered 3.7 Variants**: Full registration for `gemini-3.7-flash`, `gemini-3.7-flash-low`, `gemini-3.7-flash-medium`, `gemini-3.7-flash-high`, and `gemini-3.7-flash-tiered`.
            -   **Eliminated False 429 Outages**: Fixed account quota scheduler falsely intercepting requests with "All accounts limited" on unregistered 3.7 variants.
    *   **v4.5.9 (2026-08-23)**:
        -   **[Core Feature] OpenAI Compatible Endpoint Multimodal Audio Input Support (PR #3321)**:
            -   **Standard Audio Formats**: Supports OpenAI official `input_audio` (Base64 + format) and `audio_url`, converting seamlessly to Gemini `inlineData`/`fileData`.
            -   **Normalization**: Normalizes `wav`, `mp3`, `m4a`, `ogg`, `flac`, `aiff` from Data URLs, remote HTTP links, local files, and raw Base64.
        -   **[Core Fix] OAuth Token Refresh Resilience & Backoff (PR #3321)**:
            -   **Proactive Buffer (5 Min)**: Increased token refresh window from 90s to 300s ahead of expiry.
            -   **Backoff Retry & Consecutive Failure Gate**: Retries after 500ms backoff on `invalid_grant` and disables accounts only after 2+ consecutive confirmed failures.
        -   **[Core Fix] 403 / VALIDATION_REQUIRED Detection & URL Parsing**:
            -   **Validation URL Extraction**: Parses `validation_url` / `appeal_url` from Google RPC responses and flags accounts in the UI with a quick-action verification button.
