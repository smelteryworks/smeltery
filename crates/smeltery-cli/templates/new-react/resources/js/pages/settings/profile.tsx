import { useForm, usePage } from '@inertiajs/react';
import type { FormEvent } from 'react';

import TextInput from '@/components/text-input';
import SettingsLayout from '@/layouts/settings-layout';

/** The signed-in user's name and e-mail address (`PUT /user/profile-information`). */
export default function Profile() {
    const user = usePage().props.auth.user;
    const form = useForm({ name: user?.name ?? '', email: user?.email ?? '' });

    function submit(event: FormEvent) {
        event.preventDefault();
        form.put('/user/profile-information', { preserveScroll: true });
    }

    return (
        <SettingsLayout title="Profile" current="/settings/profile">
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <TextInput
                    name="name"
                    label="Name"
                    value={form.data.name}
                    onChange={(e) => form.setData('name', e.target.value)}
                    error={form.errors.name}
                    required
                    autoComplete="name"
                />
                <TextInput
                    name="email"
                    label="Email"
                    type="email"
                    value={form.data.email}
                    onChange={(e) => form.setData('email', e.target.value)}
                    error={form.errors.email}
                    required
                    autoComplete="username"
                />
                <button type="submit" className="btn-primary" disabled={form.processing}>
                    Save
                </button>
            </form>
        </SettingsLayout>
    );
}
