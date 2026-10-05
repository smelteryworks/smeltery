<!-- The public `announcements` channel, live: the newest five announcements, as the server broadcasts them. -->
<script setup lang="ts">
import { useEchoPublic } from '@laravel/echo-vue';
import { ref } from 'vue';

import AppCard from '@/components/AppCard.vue';

/** `AnnouncementPosted` in `app/events/announcement_posted.rs`: the event's data. */
interface AnnouncementPosted {
    message: string;
}

const messages = ref<string[]>([]);
useEchoPublic<AnnouncementPosted>('announcements', 'AnnouncementPosted', (event) => {
    messages.value = [event.message, ...messages.value].slice(0, 5);
});
</script>

<template>
    <AppCard title="Announcements">
        <p v-if="messages.length === 0">
            Live from the <code>announcements</code> channel: a handler, a job or an agent sends one with
            <code>anvil.send(&amp;AnnouncementPosted { message })</code>.
        </p>
        <ul v-else class="space-y-1" aria-live="polite">
            <li v-for="(message, i) in messages" :key="i">{{ message }}</li>
        </ul>
    </AppCard>
</template>
