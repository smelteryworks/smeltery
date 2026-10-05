<!-- The second step of a login with two-factor authentication: a code from the app, or a recovery code. -->
<script setup lang="ts">
import { useForm } from '@inertiajs/vue3';
import { ref } from 'vue';

import TextInput from '@/components/TextInput.vue';
import AuthLayout from '@/layouts/AuthLayout.vue';

const recovery = ref(false);
const form = useForm({ code: '', recovery_code: '' });

function submit() {
    form.transform((data) => (recovery.value ? { recovery_code: data.recovery_code } : { code: data.code })).post('/two-factor-challenge', {
        onFinish: () => form.reset(),
    });
}

function toggle() {
    recovery.value = !recovery.value;
    form.clearErrors();
    form.reset();
}
</script>

<template>
    <AuthLayout title="Two-factor authentication" :description="recovery ? 'Enter one of your recovery codes.' : 'Enter the code from your authenticator app.'">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput
                v-if="recovery"
                key="recovery_code"
                v-model="form.recovery_code"
                name="recovery_code"
                label="Recovery code"
                :error="form.errors.recovery_code"
                required
                autofocus
                autocomplete="off"
            />
            <TextInput
                v-else
                key="code"
                v-model="form.code"
                name="code"
                label="Code"
                inputmode="numeric"
                pattern="[0-9]*"
                maxlength="6"
                :error="form.errors.code"
                required
                autofocus
                autocomplete="one-time-code"
            />
            <button type="submit" class="btn-primary w-full" :disabled="form.processing">Log in</button>
            <p class="text-sm">
                <button type="button" class="link" @click="toggle">
                    {{ recovery ? 'Use a code from the app instead' : 'Use a recovery code instead' }}
                </button>
            </p>
        </form>
    </AuthLayout>
</template>
