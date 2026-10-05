/** A `Photo` record as `app/controllers/photos.rs` sends it (the fields of `app/models/photo.rs`). */
export interface Photo {
    id: number;
    title: string;
    image: string;
    scan: string | null;
    public: boolean;
    created_at: string | null;
    updated_at: string | null;
}
