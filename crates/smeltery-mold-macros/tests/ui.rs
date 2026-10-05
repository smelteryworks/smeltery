//! Compile-fail tests: template mistakes are compile errors that name the `.mold.html` file and line.

#[test]
fn ui() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
