mod catalog;
mod memory;
fn main() {
    let group = std::env::args().nth(1).unwrap_or("all".into());
    if group == "all" || group == "memory" {
        memory::run();
    }
    if group == "all" || group == "catalog" {
        catalog::run();
    }
}
