<!-- A field's validation message (`form.errors.email`), linked to the field by `id` (`<field>-error`). -->
<script setup lang="ts">
defineProps<{ id: string; message?: string }>();
</script>

<template>
    <p v-if="message" :id="id" class="form-error">{{ message }}</p>
</template>
