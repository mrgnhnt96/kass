//! Prints the cleanup pipeline's id (`kass_text::pipeline::id`), for the
//! shared-adapter scripts (scripts/shared-adapters) to stamp and check with.

fn main() {
    println!("{}", kass_text::pipeline::id());
}
