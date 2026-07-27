use std::{
    env, fs,
    path::{Path, PathBuf},
};

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).expect("create embedded UI directory");
    for entry in fs::read_dir(from).expect("read UI distribution") {
        let entry = entry.expect("read UI entry");
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if source.is_dir() {
            copy_tree(&source, &destination);
        } else {
            fs::copy(source, destination).expect("copy UI asset");
        }
    }
}

fn main() {
    println!("cargo:rerun-if-env-changed=WOTBOX_UI_DIST");
    println!("cargo:rerun-if-changed=frontend/src");
    let output = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR")).join("ui");
    if output.exists() {
        fs::remove_dir_all(&output).expect("clear embedded UI directory");
    }

    match env::var_os("WOTBOX_UI_DIST").map(PathBuf::from) {
        Some(source) if source.join("index.html").exists() => copy_tree(&source, &output),
        _ => {
            fs::create_dir_all(&output).expect("create fallback UI directory");
            fs::write(
                output.join("index.html"),
                "<!doctype html><title>Wotbox</title><p>Build the Svelte frontend to use the web interface.</p>",
            )
            .expect("write fallback UI");
        }
    }
}
