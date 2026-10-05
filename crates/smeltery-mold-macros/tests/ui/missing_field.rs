use smeltery_mold_macros::Mold;

#[derive(Mold)]
#[mold("missing_field", crate = "smeltery_mold", dir = "../../../../crates/smeltery-mold-macros/tests/ui/views")]
struct Page {
    title: String,
}

fn main() {}
