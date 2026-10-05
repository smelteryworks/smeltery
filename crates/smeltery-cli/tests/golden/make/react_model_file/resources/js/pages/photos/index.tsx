import { Head, Link } from '@inertiajs/react';

import type { Photo } from '@/types/photo';

/** `GET /photos` (`index` in `app/controllers/photos.rs`): every photo. */
export default function Index({ photos }: { photos: Photo[] }) {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Photos" />
            <div className="flex flex-wrap items-center justify-between gap-4">
                <h1 className="text-3xl font-bold tracking-tight">Photos</h1>
                <Link href="/photos/create" className="btn-primary">
                    New photo
                </Link>
            </div>
            <ul className="panel mt-8 divide-y divide-ash-200 p-0 dark:divide-forge-800">
                {photos.map((photo) => (
                    <li key={photo.id}>
                        <Link
                            href={`/photos/${photo.id}`}
                            className="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800"
                        >
                            {photo.title}
                        </Link>
                    </li>
                ))}
                {photos.length === 0 && <li className="px-6 py-4 text-stone-600 dark:text-stone-400">No photos yet.</li>}
            </ul>
        </main>
    );
}
