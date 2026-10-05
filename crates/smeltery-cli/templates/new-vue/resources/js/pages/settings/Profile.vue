<!-- The signed-in user's name and e-mail address (`PUT /user/profile-information`). -->
<script setup lang="ts">
import { useForm, usePage } from '@inertiajs/vue3';

import TextInput from '@/components/TextInput.vue';
import SettingsLayout from '@/layouts/SettingsLayout.vue';

const user = usePage().props.auth.user;
const form = useForm({ name: user?.name ?? '', email: user?.email ?? '' });

function submit() {
    form.put('/user/profile-information', { preserveScroll: true });
}
</script>

<template>
    <SettingsLayout title="Profile" current="/settings/profile">
        <form class="panel mt-8 space-y-5" @submit.prevent="submit">
            <TextInput v-model="form.name" name="name" label="Name" :error="form.errors.name" required autocomplete="name" />
            <TextInput v-model="form.email" name="email" label="Email" type="email" :error="form.errors.email" required autocomplete="username" />
            <button type="submit" class="btn-primary" :disabled="form.processing">Save</button>
        </form>
    </SettingsLayout>
</template>
