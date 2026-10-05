<!--
The signed-in user's private channel `users.<id>` (only they may join it, `routes/channels.rs`) and the presence
channel `dashboard` (how many people have the dashboard open). "Notify me" asks the server to send this user an event.
-->
<script setup lang="ts">
import { router, usePage } from '@inertiajs/vue3';
import { echo, useEcho } from '@laravel/echo-vue';
import { computed, onMounted, onUnmounted, ref } from 'vue';

import AppCard from '@/components/AppCard.vue';

/** `UserNotified` in `app/events/user_notified.rs`: the event's data. */
interface UserNotified {
    message: string;
}

/** A member of `presence-dashboard`: `routes/channels.rs` shares only the id (add `name` there to list names). */
interface Member {
    id: number;
}

const page = usePage();
const id = computed(() => page.props.auth.user?.id ?? 0);
const name = computed(() => page.props.auth.user?.name ?? '');
const messages = ref<string[]>([]);
const online = ref<Member[]>([]);
const sending = ref(false);

useEcho<UserNotified>(`users.${id.value}`, 'UserNotified', (event) => {
    messages.value = [event.message, ...messages.value].slice(0, 5);
});

onMounted(() => {
    echo()
        .join('dashboard')
        .here((members: Member[]) => (online.value = members))
        .joining((member: Member) => (online.value = [...online.value.filter((m) => m.id !== member.id), member]))
        .leaving((member: Member) => (online.value = online.value.filter((m) => m.id !== member.id)));
});
onUnmounted(() => echo().leave('dashboard'));

function notifyMe() {
    router.post('/notify-me', {}, { preserveScroll: true, onStart: () => (sending.value = true), onFinish: () => (sending.value = false) });
}
</script>

<template>
    <AppCard title="Live">
        <p class="font-medium">
            {{ online.length > 0 ? `${online.length} ${online.length === 1 ? 'person' : 'people'} online · you: ${name}` : 'connecting…' }}
        </p>
        <button type="button" class="btn-secondary mt-3" :disabled="sending" @click="notifyMe">Notify me</button>
        <ul class="mt-3 space-y-1" aria-live="polite">
            <li v-for="(message, i) in messages" :key="i">{{ message }}</li>
        </ul>
    </AppCard>
</template>
