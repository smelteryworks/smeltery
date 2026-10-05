import { Head, Link, useForm } from '@inertiajs/react';
import type { FormEvent } from 'react';

import type { Photo } from '@/types/photo';

/** `GET /photos/{id}/edit` (`edit` in `app/controllers/photos.rs`): the form that `update` saves. */
export default function Edit({ photo }: { photo: Photo }) {
    const form = useForm({
        title: photo.title,
        image: null as File | null,
        scan: null as File | null,
        public: photo.public,
    });

    function submit(event: FormEvent) {
        event.preventDefault();
        // With a file the form goes up as multipart/form-data, where a checkbox is the text `true` or `false`.
        form.transform((data) => ({
            ...data,
            public: String(data.public),
        }));
        form.put(`/photos/${photo.id}`, { forceFormData: true });
    }

    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Edit photo" />
            <h1 className="text-3xl font-bold tracking-tight">
                Edit photo {photo.id}
            </h1>
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
                    <label htmlFor="image" className="form-label">
                        Image
                    </label>
                    <input
                        id="image"
                        name="image"
                        type="file"
                        className="form-input"
                        onChange={(e) => form.setData('image', e.target.files?.[0] ?? null)}
                        aria-invalid={form.errors.image ? true : undefined}
                        aria-describedby={form.errors.image ? 'image-error' : undefined}
                    />
                    {form.errors.image && (
                        <p id="image-error" className="form-error">
                            {form.errors.image}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="scan" className="form-label">
                        Scan
                    </label>
                    <input
                        id="scan"
                        name="scan"
                        type="file"
                        className="form-input"
                        onChange={(e) => form.setData('scan', e.target.files?.[0] ?? null)}
                        aria-invalid={form.errors.scan ? true : undefined}
                        aria-describedby={form.errors.scan ? 'scan-error' : undefined}
                    />
                    {form.errors.scan && (
                        <p id="scan-error" className="form-error">
                            {form.errors.scan}
                        </p>
                    )}
                </div>
                <div>
                    <label htmlFor="public" className="form-label">
                        Public
                    </label>
                    <input
                        id="public"
                        name="public"
                        type="checkbox"
                        className="mt-2 h-4 w-4 accent-molten-700"
                        checked={form.data.public}
                        onChange={(e) => form.setData('public', e.target.checked)}
                        aria-invalid={form.errors.public ? true : undefined}
                        aria-describedby={form.errors.public ? 'public-error' : undefined}
                    />
                    {form.errors.public && (
                        <p id="public-error" className="form-error">
                            {form.errors.public}
                        </p>
                    )}
                </div>
                <div className="flex items-center gap-3">
                    <button type="submit" className="btn-primary" disabled={form.processing}>
                        Save
                    </button>{' '}
                    <Link href="/photos" className="btn-secondary">
                        Cancel
                    </Link>
                </div>
            </form>
        </main>
    );
}
