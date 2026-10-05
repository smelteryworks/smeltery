import { Head, Link } from '@inertiajs/react';

import type { Post } from '@/types/post';

/** `GET /posts` (`index` in `app/controllers/posts.rs`): every post. */
export default function Index({ posts }: { posts: Post[] }) {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Posts" />
            <div className="flex flex-wrap items-center justify-between gap-4">
                <h1 className="text-3xl font-bold tracking-tight">Posts</h1>
                <Link href="/posts/create" className="btn-primary">
                    New post
                </Link>
            </div>
            <ul className="panel mt-8 divide-y divide-ash-200 p-0 dark:divide-forge-800">
                {posts.map((post) => (
                    <li key={post.id}>
                        <Link
                            href={`/posts/${post.id}`}
                            className="block px-6 py-4 font-medium hover:bg-ash-100 focus-visible:outline-2 focus-visible:outline-molten-500 dark:hover:bg-forge-800"
                        >
                            {post.title}
                        </Link>
                    </li>
                ))}
                {posts.length === 0 && <li className="px-6 py-4 text-stone-600 dark:text-stone-400">No posts yet.</li>}
            </ul>
        </main>
    );
}
