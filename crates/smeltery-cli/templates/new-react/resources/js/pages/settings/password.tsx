import { useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import SettingsLayout from '@/layouts/settings-layout';

/** A new password (`PUT /user/password`): other devices are signed out, this one stays signed in. */
export default function Password() {
    const form = useForm({ current_password: '', password: '', password_confirmation: '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.put('/user/password', {
            preserveScroll: true,
            onFinish: () => form.reset(),
        });
    }

    return (
        <SettingsLayout title="Password" current="/settings/password">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="current_password"
                    label="Current password"
                    type="password"
                    value={form.data.current_password}
                    onChange={(e) => form.setData('current_password', e.target.value)}
                    error={form.errors.current_password}
                    required
                    autoComplete="current-password"
                />
                <TextInput
                    name="password"
                    label="New password"
                    type="password"
                    value={form.data.password}
                    onChange={(e) => form.setData('password', e.target.value)}
                    error={form.errors.password}
                    required
                    autoComplete="new-password"
                />
                <TextInput
                    name="password_confirmation"
                    label="Confirm password"
                    type="password"
                    value={form.data.password_confirmation}
                    onChange={(e) => form.setData('password_confirmation', e.target.value)}
                    error={form.errors.password_confirmation}
                    required
                    autoComplete="new-password"
                />
                <p className="text-sm text-stone-600 dark:text-stone-400">Your other devices are signed out; this one stays signed in.</p>
                <button type="submit" className="btn-primary" disabled={form.processing}>
                    Change password
                </button>
            </form>
        </SettingsLayout>
    );
}
