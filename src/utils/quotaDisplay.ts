import type { ModelQuota, QuotaGroup } from '../types/account';

// An exhausted weekly window takes precedence; protection still uses the backend quota.
export function getModelQuotaDisplay(modelId: string, model: ModelQuota | undefined, groups: QuotaGroup[] = []) {
    const name = modelId.toLowerCase();
    const thirdParty = name.startsWith('claude') || name.startsWith('gpt');
    const buckets = groups.filter(group => {
        const groupName = group.display_name.toLowerCase();
        const isThirdParty = /claude|gpt|3p/.test(groupName)
            || group.buckets.some(bucket => bucket.bucket_id.toLowerCase().includes('3p'));
        return thirdParty ? isThirdParty : name.startsWith('gemini') && !isThirdParty;
    }).flatMap(group => group.buckets);
    const fiveHour = buckets.filter(bucket => /5h|hour/i.test(`${bucket.window} ${bucket.bucket_id}`))
        .reduce<(typeof buckets)[number] | undefined>((chosen, bucket) =>
            !chosen || bucket.remaining_fraction < chosen.remaining_fraction ? bucket : chosen, undefined);
    const weekly = buckets.filter(bucket => /week|7d/i.test(`${bucket.window} ${bucket.bucket_id}`)
        && bucket.remaining_fraction <= 0.001 && Date.parse(bucket.reset_time) > Date.now())
        .reduce<(typeof buckets)[number] | undefined>((chosen, bucket) =>
            !chosen || Date.parse(bucket.reset_time) > Date.parse(chosen.reset_time) ? bucket : chosen, undefined);
    return {
        percentage: weekly ? 0 : fiveHour ? Math.round(fiveHour.remaining_fraction * 100) : (model?.percentage ?? 0),
        resetTime: weekly?.reset_time || fiveHour?.reset_time || model?.reset_time,
        isWeeklyConstrained: !!weekly,
        weeklyResetTime: weekly?.reset_time,
    };
}
