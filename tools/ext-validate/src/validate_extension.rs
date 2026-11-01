#![allow(linker_messages)]
fn main() {
    if let Err(error) = loom_ext_validate::validate_extension() {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
