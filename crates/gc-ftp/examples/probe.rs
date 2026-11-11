//! `cargo run -p gc-ftp --example probe -- ftps://user:pass@host/` : connects, lists, prints the connection log.

fn main() {
    let url = std::env::args().nth(1).expect("usage: probe <ftp(s)://user:pass@host[:port]/dir>");
    let mut target = gc_ftp::Target::from_url(&url).unwrap();
    target.insecure_tls = std::env::args().any(|a| a == "--insecure");
    match gc_ftp::connect_and_list(target) {
        Ok((_, path, entries)) => println!("OK {path}: {} entries", entries.len()),
        Err(e) => println!("FAILED: {e}"),
    }
    for line in gc_ftp::log_lines() {
        println!("{line}");
    }
}
