//! Prints the scanner's findings for each skill directory given on the command
//! line. Read-only: the scanner never runs anything it finds.
fn main() {
    for arg in std::env::args().skip(1) {
        match zerith_skills::scan(std::path::Path::new(&arg)) {
            Err(e) => println!("{arg}: ERRO {e}"),
            Ok(s) => {
                println!("{} ({} scripts)", s.frontmatter.name, s.scripts.len());
                for f in s.findings {
                    println!("   [{}] {}", f.severity.as_str(), f.message);
                }
            }
        }
    }
}
