fn main() {
    // Set a custom environment variable for rob
    println!("cargo:rustc-env=ROB_VERSION=0.1.0");
    println!("cargo:rerun-if-changed=build.rs"); // rebuild if build.rs changes
}
