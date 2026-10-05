import { Head, Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

/** `GET /posts/create` (`create` in `app/controllers/posts.rs`): the form that `store` saves. */
export default function Create() {
    const form = useForm({
        title: '',
        body: '',
        user_id: '',
    });

    function submit(event: FormEvent) {
        event.preventDefault();
        // Number fields hold the text of their input; the server reads numbers (an empty field is a missing value).
        form.transform((data) => ({
            ...data,
            user_id: data.user_id === '' ? '' : Number(data.user_id),
        }));
        form.post('/posts');
    }

    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="New post" />
            <h1 className="text-3xl font-bold tracking-tight">New post</h1>
            <form onSubmit={submit} className="panel mt-8 space-y-5">
                <div>
                    <label htmlFor="title" className="form-label">
                        Title
                    </label>
                    <input
                        id="title"
                        name="title"
                        type="text"
                        required
                        className="form-input"
                        value={form.data.title}
                        onChange={(e) => form.setData('title', e.target.value)}
                        aria-invalid={form.errors.title ? true : undefined}
                        aria-describedby={form.errors.title ? 'title-error' : undefined}
                    />
                    {form.errors.title && (
                        <p id="title-error" className="form-error">
                            {form.errors.title}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="body" className="form-label">
                        Body
                    </label>
                    <textarea
                        id="body"
                        name="body"
                        className="form-input"
                        value={form.data.body}
                        onChange={(e) => form.setData('body', e.target.value)}
                        aria-invalid={form.errors.body ? true : undefined}
                        aria-describedby={form.errors.body ? 'body-error' : undefined}
                    />
                    {form.errors.body && (
                        <p id="body-error" className="form-error">
                            {form.errors.body}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="user_id" className="form-label">
                        User id
                    </label>
                    <input
                        id="user_id"
                        name="user_id"
                        type="number"
                        className="form-input"
                        value={form.data.user_id}
                        onChange={(e) => form.setData('user_id', e.target.value)}
                        aria-invalid={form.errors.user_id ? true : undefined}
                        aria-describedby={form.errors.user_id ? 'user_id-error' : undefined}
                    />
                    {form.errors.user_id && (
                        <p id="user_id-error" className="form-error">
                            {form.errors.user_id}
                        </p>
                    )}
                </div>
                <div className="flex items-center gap-3">
                    <button type="submit" className="btn-primary" disabled={form.processing}>
                        Save
                    </button>{' '}
                    <Link href="/posts" className="btn-secondary">
                        Cancel
                    </Link>
                </div>
            </form>
        </main>
    );
}
