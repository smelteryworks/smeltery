import { Head, Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

/** `GET /posts/create` (`create` in `app/controllers/posts.rs`): the form that `store` saves. */
export default function Create() {
    const form = useForm({
        title: '',
        body: '',
        views: '',
        rating: '',
        published: false,
    });

    function submit(event: FormEvent) {
        event.preventDefault();
        // Number fields hold the text of their input; the server reads numbers (an empty field is a missing value).
        form.transform((data) => ({
            ...data,
            views: data.views === '' ? '' : Number(data.views),
            rating: data.rating === '' ? '' : Number(data.rating),
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
                    <label htmlFor="views" className="form-label">
                        Views
                    </label>
                    <input
                        id="views"
                        name="views"
                        type="number"
                        required
                        className="form-input"
                        value={form.data.views}
                        onChange={(e) => form.setData('views', e.target.value)}
                        aria-invalid={form.errors.views ? true : undefined}
                        aria-describedby={form.errors.views ? 'views-error' : undefined}
                    />
                    {form.errors.views && (
                        <p id="views-error" className="form-error">
                            {form.errors.views}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="rating" className="form-label">
                        Rating
                    </label>
                    <input
                        id="rating"
                        name="rating"
                        type="number"
                        step="any"
                        className="form-input"
                        value={form.data.rating}
                        onChange={(e) => form.setData('rating', e.target.value)}
                        aria-invalid={form.errors.rating ? true : undefined}
                        aria-describedby={form.errors.rating ? 'rating-error' : undefined}
                    />
                    {form.errors.rating && (
                        <p id="rating-error" className="form-error">
                            {form.errors.rating}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="published" className="form-label">
                        Published
                    </label>
                    <input
                        id="published"
                        name="published"
                        type="checkbox"
                        className="mt-2 h-4 w-4 accent-molten-700"
                        checked={form.data.published}
                        onChange={(e) => form.setData('published', e.target.checked)}
                        aria-invalid={form.errors.published ? true : undefined}
                        aria-describedby={form.errors.published ? 'published-error' : undefined}
                    />
                    {form.errors.published && (
                        <p id="published-error" className="form-error">
                            {form.errors.published}
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
