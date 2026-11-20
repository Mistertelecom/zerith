fn main() {
    println!("cargo:rerun-if-changed=update-pubkey.txt");
    println!("cargo:rerun-if-env-changed=ZERITH_UPDATE_PUBKEY");
    tauri_build::build()
}
