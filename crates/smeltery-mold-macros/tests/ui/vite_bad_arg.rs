use smeltery_mold_macros::Mold;

#[derive(Mold)]
#[mold("vite_bad_arg", crate = "smeltery_mold", dir = "../../../../crates/smeltery-mold-macros/tests/ui/views")]
struct Page {
    title: String,
}

fn main() {}
