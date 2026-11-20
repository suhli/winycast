fn main() {
    println!("cargo:rerun-if-changed=winycast.rc");
    println!("cargo:rerun-if-changed=winycast.manifest");
    embed_resource::compile("winycast.rc", embed_resource::NONE)
        .manifest_required()
        .expect("could not embed the Windows application manifest");
}
