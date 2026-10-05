<!-- The welcome page's forge: an isometric ingot over an ember glow, with sparks rising in CSS (`.spark`). -->
<script setup lang="ts">
/** Where each spark rises: horizontal position, duration, delay and sideways drift. */
const sparks = [
    { x: '30%', t: '3.2s', d: '0s', drift: '-18px' },
    { x: '42%', t: '2.6s', d: '0.8s', drift: '12px' },
    { x: '50%', t: '3.6s', d: '1.6s', drift: '-6px' },
    { x: '58%', t: '2.9s', d: '0.4s', drift: '20px' },
    { x: '66%', t: '3.4s', d: '2.1s', drift: '-12px' },
    { x: '36%', t: '4s', d: '2.7s', drift: '24px' },
    { x: '62%', t: '3.8s', d: '1.2s', drift: '-22px' },
];
</script>

<template>
    <div class="relative mx-auto aspect-square w-full max-w-60 sm:max-w-sm" aria-hidden="true">
        <div class="forge-glow absolute inset-6 rounded-full"></div>
        <svg class="absolute inset-0 m-auto w-3/5 drop-shadow-[0_0_30px_rgb(255_122_31/0.55)]" viewBox="0 0 120 100">
            <defs>
                <linearGradient id="ingot-top" x1="0" y1="0" x2="1" y2="1">
                    <stop offset="0" stop-color="#ffe2a8" />
                    <stop offset="1" stop-color="#ffb84d" />
                </linearGradient>
                <linearGradient id="ingot-left" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0" stop-color="#ff7a52" />
                    <stop offset="1" stop-color="#c72e16" />
                </linearGradient>
                <linearGradient id="ingot-right" x1="0" y1="0" x2="0" y2="1">
                    <stop offset="0" stop-color="#e8361d" />
                    <stop offset="1" stop-color="#7a1c0b" />
                </linearGradient>
            </defs>
            <path d="M60 20 104 40 60 60 16 40z" fill="url(#ingot-top)" />
            <path d="M16 40 60 60v24L16 64z" fill="url(#ingot-left)" />
            <path d="M104 40 60 60v24l44-20z" fill="url(#ingot-right)" />
        </svg>
        <span
            v-for="s in sparks"
            :key="s.x + s.d"
            class="spark"
            :style="{ '--x': s.x, '--t': s.t, '--d': s.d, '--drift': s.drift }"
        ></span>
    </div>
</template>
