<!--
Turn two-factor authentication on (scan the QR code, confirm with a first code), see the recovery codes once, make
new ones, or turn it off. The QR code and key are fetched from `/user/two-factor-qr-code` and
`/user/two-factor-secret-key` while enrolling, so they never sit in the page's props or the browser's history. The
recovery codes arrive once as a prop (on the first visit or after a form post); the page keeps its own copy and drops
them from the props (and so from the history entry) at once, so the back button does not show them again.
-->
<script setup lang="ts">
import { router, useForm } from '@inertiajs/vue3';
import { computed, nextTick, ref, watch } from 'vue';

import TextInput from '@/components/TextInput.vue';
import SettingsLayout from '@/layouts/SettingsLayout.vue';

/** `TwoFactorState` in `app/controllers/settings.rs`. */
interface TwoFactorState {
    enabled: boolean;
    confirmed: boolean;
    recovery_codes_left: number;
}

/** What an authenticator app needs while enrolling: the QR code (an SVG data URL) and the key for typing in. */
interface Setup {
    qrCodeUrl: string;
    secretKey: string;
}

const props = defineProps<{ two_factor: TwoFactorState; recovery_codes: string[] }>();

const enrolling = computed(() => props.two_factor.enabled && !props.two_factor.confirmed);
const setup = ref<Setup | null>(null);
const enable = useForm({});
const confirm = useForm({ code: '' });
const regenerate = useForm({});
const disable = useForm({});
const codes = ref<string[]>([]);

// Inertia keeps this component across the page's own form posts ("Turn on", "New recovery codes"), so new codes
// arrive as a changed prop, not a new mount: take every non-empty set, then drop it from the props and the history
// entry (the empty prop that follows changes nothing here).
watch(
    () => props.recovery_codes,
    (fresh) => {
        if (fresh.length > 0) {
            codes.value = [...fresh];
            // After the render (the first call runs while the page is still being set up).
            void nextTick(() => router.replaceProp('recovery_codes', []));
        }
    },
    { immediate: true },
);

// Turned off: the old codes no longer work, so they leave the page.
watch(
    () => props.two_factor.enabled,
    (enabled) => {
        if (!enabled) {
            codes.value = [];
        }
    },
);

/** Fetches a JSON route of Temper with the session cookie. */
async function json(url: string): Promise<Record<string, unknown>> {
    const response = await fetch(url, { headers: { Accept: 'application/json' } });
    if (!response.ok) {
        throw new Error(`${url}: ${response.status}`);
    }
    return (await response.json()) as Record<string, unknown>;
}

watch(
    enrolling,
    async (now, _before, onCleanup) => {
        setup.value = null;
        if (!now) {
            return;
        }
        let current = true;
        onCleanup(() => {
            current = false;
        });
        try {
            const [qr, key] = await Promise.all([json('/user/two-factor-qr-code'), json('/user/two-factor-secret-key')]);
            const url = String(qr.url ?? '');
            // Only the image Temper draws: never another kind of URL in `src`.
            if (current && url.startsWith('data:image/svg+xml;base64,')) {
                setup.value = { qrCodeUrl: url, secretKey: String(key.secretKey ?? '') };
            }
        } catch {
            if (current) {
                setup.value = null;
            }
        }
    },
    { immediate: true },
);

function submitConfirm() {
    confirm.post('/user/confirmed-two-factor-authentication', { preserveScroll: true, onFinish: () => confirm.reset() });
}
</script>

<template>
    <SettingsLayout title="Two-factor authentication" current="/settings/two-factor">
        <section class="panel mt-8 space-y-5 text-sm text-stone-700 dark:text-stone-300">
            <p v-if="two_factor.confirmed">
                <strong>On.</strong> Logging in asks for a code from your authenticator app. Recovery codes left:
                <span class="tabular-nums">{{ two_factor.recovery_codes_left }}</span>.
            </p>
            <template v-if="enrolling">
                <p>Scan the QR code with your authenticator app (or type the key), then enter the code it shows to finish.</p>
                <template v-if="setup">
                    <img :src="setup.qrCodeUrl" alt="QR code for your authenticator app" class="w-48 h-48 rounded-lg bg-white" />
                    <p>
                        Key: <code class="font-mono break-all">{{ setup.secretKey }}</code>
                    </p>
                </template>
                <form class="space-y-5" @submit.prevent="submitConfirm">
                    <TextInput
                        v-model="confirm.code"
                        name="code"
                        label="Code"
                        inputmode="numeric"
                        pattern="[0-9]*"
                        maxlength="6"
                        :error="confirm.errors.code"
                        required
                        autocomplete="one-time-code"
                    />
                    <button type="submit" class="btn-primary" :disabled="confirm.processing">Confirm</button>
                </form>
            </template>
            <template v-if="!two_factor.enabled">
                <p>
                    <strong>Off.</strong> With two-factor authentication on, logging in also asks for a code from an authenticator app on your
                    phone.
                </p>
                <button
                    type="button"
                    class="btn-primary"
                    :disabled="enable.processing"
                    @click="enable.post('/user/two-factor-authentication', { preserveScroll: true })"
                >
                    Turn on
                </button>
            </template>
            <div v-if="codes.length > 0" class="space-y-3">
                <p>
                    <strong>Recovery codes.</strong> Store them somewhere safe: each one logs you in once without the app, and this page shows
                    them only now.
                </p>
                <ul class="grid gap-2 font-mono sm:grid-cols-2">
                    <li v-for="code in codes" :key="code">{{ code }}</li>
                </ul>
            </div>
            <button
                v-if="two_factor.confirmed"
                type="button"
                class="btn-secondary"
                :disabled="regenerate.processing"
                @click="regenerate.post('/user/two-factor-recovery-codes', { preserveScroll: true })"
            >
                New recovery codes
            </button>
            <p v-if="two_factor.enabled">
                <button
                    type="button"
                    class="btn-secondary"
                    :disabled="disable.processing"
                    @click="disable.delete('/user/two-factor-authentication', { preserveScroll: true })"
                >
                    Turn off
                </button>
            </p>
        </section>
    </SettingsLayout>
</template>
