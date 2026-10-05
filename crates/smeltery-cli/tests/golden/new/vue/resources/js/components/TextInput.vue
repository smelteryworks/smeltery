<!-- A labelled form field; with an error it is marked `aria-invalid` and described by the message under it. -->
<script setup lang="ts">
import InputError from '@/components/InputError.vue';

defineOptions({ inheritAttrs: false });
defineProps<{
    /** The field name, also its `id`. */
    name: string;
    label: string;
    /** The validation message for this field, if any. */
    error?: string;
}>();
const model = defineModel<string>({ required: true });
</script>

<template>
    <div>
        <label :for="name" class="form-label">{{ label }}</label>
        <input
            :id="name"
            v-model="model"
            :name="name"
            class="form-input"
            :aria-invalid="error ? true : undefined"
            :aria-describedby="error ? `${name}-error` : undefined"
            v-bind="$attrs"
        />
        <InputError :id="`${name}-error`" :message="error" />
    </div>
</template>
