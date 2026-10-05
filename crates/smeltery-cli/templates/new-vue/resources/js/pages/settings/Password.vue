<!-- A new password (`PUT /user/password`): other devices are signed out, this one stays signed in. -->
<script setup lang="ts">
import { useForm } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import SettingsLayout from '@/layouts/SettingsLayout.vue';

const form = useForm({ current_password: '', password: '', password_confirmation: '' });

function submit() {
    form.put('/user/password', { preserveScroll: true, onFinish: () => form.reset() });
}
</script>

<template>
    <SettingsLayout title="Password" current="/settings/password">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput
                v-model="form.current_password"
                name="current_password"
                label="Current password"
                type="password"
                :error="form.errors.current_password"
                required
                autocomplete="current-password"
            />
            <TextInput v-model="form.password" name="password" label="New password" type="password" :error="form.errors.password" required autocomplete="new-password" />
            <TextInput
                v-model="form.password_confirmation"
                name="password_confirmation"
                label="Confirm password"
                type="password"
                :error="form.errors.password_confirmation"
                required
                autocomplete="new-password"
            />
            <p class="text-sm text-stone-600 dark:text-stone-400">Your other devices are signed out; this one stays signed in.</p>
            <button type="submit" class="btn-primary" :disabled="form.processing">Change password</button>
        </form>
    </SettingsLayout>
</template>
