fn main() {
    println!("cargo:rerun-if-env-changed=AHRARA_SERVER_PUBLIC_KEY");
}
