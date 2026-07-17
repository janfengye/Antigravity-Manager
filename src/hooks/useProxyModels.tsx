import { useMemo, useEffect, useState } from 'react';
import { MODEL_CONFIG } from '../config/modelConfig';
import { useAccountStore } from '../stores/useAccountStore';
import { Bot } from 'lucide-react';
import { useTranslation } from 'react-i18next';
import { request } from '../utils/request';

export interface CanonicalFamilyDto {
    canonical_id: string;
    display_name: string;
    match_ids: string[];
}

export const useProxyModels = () => {
    const { t } = useTranslation();
    const { accounts, fetchAccounts } = useAccountStore();
    const [canonicalFamilies, setCanonicalFamilies] = useState<CanonicalFamilyDto[]>([]);

    // 确保账号数据已加载（针对未触发 fetchAccounts 的页面，如 ApiProxy）
    useEffect(() => {
        if (accounts.length === 0) {
            fetchAccounts();
        }

        let cancelled = false;
        request<CanonicalFamilyDto[]>('get_canonical_families')
            .then(data => {
                if (!cancelled && data) {
                    setCanonicalFamilies(data);
                }
            })
            .catch(err => console.error('Failed to fetch canonical families:', err));

        return () => { cancelled = true; };
    }, []); // eslint-disable-line react-hooks/exhaustive-deps

    const models = useMemo(() => {
        // Step 1: 从所有账号中收集动态模型
        // 以 name（小写）为 key 去重，优先保留含 display_name 的条目
        const dynamicMap = new Map<string, { name: string; display_name?: string }>();
        for (const account of accounts) {
            for (const m of account.quota?.models ?? []) {
                let key = m.name.toLowerCase();
                let name = m.name;
                let display_name = m.display_name;

                if (canonicalFamilies.length > 0) {
                    for (const family of canonicalFamilies) {
                        if (family.match_ids.includes(key)) {
                            key = family.canonical_id.toLowerCase();
                            name = family.canonical_id;
                            display_name = family.display_name;
                            break;
                        }
                    }
                }

                if (!dynamicMap.has(key) || display_name) {
                    dynamicMap.set(key, { name, display_name });
                }
            }
        }

        const result = [];
        const seenIds = new Set<string>();

        // Step 2: 优先展示来自账号的动态模型（display_name 为主名称，name 为 ID）
        for (const [key, m] of dynamicMap) {
            if (seenIds.has(key)) continue;
            seenIds.add(key);

            // 尝试匹配 MODEL_CONFIG 里的图标与分组
            const cfgEntry = Object.entries(MODEL_CONFIG).find(
                ([cfgId, cfg]) =>
                    cfgId.toLowerCase() === key ||
                    (cfg.protectedKey && cfg.protectedKey.toLowerCase() === key)
            );

            const primaryName = m.display_name || m.name;
            const CfgIcon = cfgEntry?.[1].Icon;
            const icon = CfgIcon
                ? <CfgIcon size={16} />
                : <Bot size={16} className="text-gray-400 dark:text-gray-500" />;
            const group = cfgEntry ? (cfgEntry[1].group || 'Other') : 'Dynamic';

            result.push({
                id: m.name,           // 原始模型 name，作为 ID 展示
                name: primaryName,    // display_name（主要展示名称）
                desc: primaryName,    // 描述栏同样用 display_name
                group,
                icon,
            });
        }

        // Step 3: 对于 MODEL_CONFIG 里有但账号未下发的型号，作为静态兜底补充
        const addedLabels = new Set<string>();
        for (const [id, config] of Object.entries(MODEL_CONFIG)) {
            const key = id.toLowerCase();
            if (seenIds.has(key)) {
                addedLabels.add((config.shortLabel || config.label).toLowerCase());
                continue;
            }
            // 跳过 thinking 变体（这类模型的动态版本由账号数据中 supports_thinking 标记覆盖）
            if (key.includes('-thinking')) continue;
            // 跳过 label 重复的别名条目
            const labelKey = (config.shortLabel || config.label).toLowerCase();
            if (addedLabels.has(labelKey)) continue;
            addedLabels.add(labelKey);
            seenIds.add(key);

            const displayName = config.i18nKey ? t(config.i18nKey, config.label) : config.label;
            result.push({
                id,
                name: displayName,
                desc: displayName,
                group: config.group || 'Other',
                icon: <config.Icon size={16} />,
            });
        }

        return result;
    }, [accounts, t, canonicalFamilies]);

    return { models, canonicalFamilies };
};
