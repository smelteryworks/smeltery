/** A `Post` record as `app/controllers/posts.rs` sends it (the fields of `app/models/post.rs`). */
export interface Post {
    id: number;
    title: string;
    body: string | null;
    views: number;
    rating: number | null;
    published: boolean;
    created_at: string | null;
    updated_at: string | null;
}
