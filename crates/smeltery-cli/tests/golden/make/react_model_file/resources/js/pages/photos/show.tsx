import { Head, Link } from '@inertiajs/react';

import type { Photo } from '@/types/photo';

/** `GET /photos/{id}` (`show` in `app/controllers/photos.rs`): one photo. */
export default function Show({ photo }: { photo: Photo }) {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Photo" />
            <h1 className="text-3xl font-bold tracking-tight">
                Photo {photo.id}
            </h1>
            <dl className="panel mt-8 grid gap-x-6 gap-y-3 sm:grid-cols-[10rem_1fr]">
                <dt className="mono-label pt-0.5">Title</dt>
                <dd>{photo.title}</dd>
                <dt className="mono-label pt-0.5">Image</dt>
                <dd>
                    {photo.image && (
                        <a href={`/storage/${photo.image}`} className="link break-all">
                            {photo.image}
                        </a>
                    )}
                </dd>
                <dt className="mono-label pt-0.5">Scan</dt>
                <dd>
                    {photo.scan && (
                        <a href={`/storage/${photo.scan}`} className="link break-all">
                            {photo.scan}
                        </a>
                    )}
                </dd>
                <dt className="mono-label pt-0.5">Public</dt>
                <dd>{photo.public ? 'Yes' : 'No'}</dd>
            </dl>
            <div className="mt-6 flex flex-wrap items-center gap-3">
                <Link href={`/photos/${photo.id}/edit`} className="btn-primary">
                    Edit
                </Link>
                <Link href="/photos" className="btn-secondary">
                    All photos
                </Link>
                <Link href={`/photos/${photo.id}`} method="delete" as="button" className="btn-secondary ml-auto text-red-700 dark:text-red-300">
                    Delete
                </Link>
            </div>
        </main>
    );
}
