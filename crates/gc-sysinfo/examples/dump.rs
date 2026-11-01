fn main() {
    print!("{}", gc_sysinfo::collect().to_text());
}
