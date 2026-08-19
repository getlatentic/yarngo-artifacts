//! Make cargo aware that the binary depends on the locale file.
//!
//! `rust_i18n::i18n!` reads `locales/` when the macro expands, but cargo only
//! tracks the sources it can see. Without this, editing copy alone never
//! triggers a rebuild and the app keeps shipping the previous strings — which
//! looks like the edit not working rather than the build not happening.
fn main() {
    println!("cargo:rerun-if-changed=locales");
    println!("cargo:rerun-if-changed=locales/app.yml");
}
