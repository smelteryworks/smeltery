import { Head, Link } from '@inertiajs/react';

import type { Post } from '@/types/post';

/** `GET /posts/{id}` (`show` in `app/controllers/posts.rs`): one post. */
export default function Show({ post }: { post: Post }) {
    return (
        <main className="mx-auto max-w-3xl px-4 py-12 sm:px-6">
            <Head title="Post" />
            <h1 className="text-3xl font-bold tracking-tight">
                Post {post.id}
            </h1>
            <dl className="panel mt-8 grid gap-x-6 gap-y-3 sm:grid-cols-[10rem_1fr]">
                <dt className="mono-label pt-0.5">Title</dt>
                <dd>{post.title}</dd>
                <dt className="mono-label pt-0.5">Body</dt>
                <dd>{post.body}</dd>
                <dt className="mono-label pt-0.5">User id</dt>
                <dd>{post.user_id}</dd>
            </dl>
            <div className="mt-6 flex flex-wrap items-center gap-3">
                <Link href={`/posts/${post.id}/edit`} className="btn-primary">
                    Edit
                </Link>
                <Link href="/posts" className="btn-secondary">
                    All posts
                </Link>
                <Link href={`/posts/${post.id}`} method="delete" as="button" className="btn-secondary ml-auto text-red-700 dark:text-red-300">
                    Delete
                </Link>
            </div>
        </main>
    );
}
