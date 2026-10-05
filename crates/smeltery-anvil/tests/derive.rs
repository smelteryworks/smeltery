//! `#[derive(BroadcastEvent)]` mistakes are compile errors with a clear message (trybuild).
#![allow(missing_docs)]

#[test]
fn derive_mistakes_do_not_compile() {
    let cases = trybuild::TestCases::new();
    cases.pass("tests/ui/pass_*.rs");
    cases.compile_fail("tests/ui/fail_*.rs");
}
