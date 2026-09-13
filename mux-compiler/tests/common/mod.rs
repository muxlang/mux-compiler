use std::fs;
use std::path::{Path, PathBuf};

pub fn runtime_library_for_child_process() -> PathBuf {
    if let Ok(path) = std::env::var("MUX_RUNTIME_LIB") {
        return PathBuf::from(path);
    }
    let static_name = if cfg!(target_family = "windows") {
        "mux_runtime.lib"
    } else {
        "libmux_runtime.a"
    };

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let runtime_roots = [
        manifest_dir.parent().map(|root| root.join("mux-runtime")),
        manifest_dir
            .parent()
            .and_then(Path::parent)
            .map(|root| root.join("mux-runtime")),
    ]
    .into_iter()
    .flatten();

    for runtime_root in runtime_roots {
        for profile in ["debug", "release"] {
            let profile_dir = runtime_root.join("target").join(profile);
            let exact = profile_dir.join(static_name);
            if exact.is_file() {
                return exact;
            }
            let mut candidates = fs::read_dir(profile_dir.join("deps"))
                .ok()
                .into_iter()
                .flatten()
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.is_file()
                        && path
                            .file_name()
                            .and_then(std::ffi::OsStr::to_str)
                            .is_some_and(|name| {
                                (name.starts_with("libmux_runtime-") && name.ends_with(".a"))
                                    || (name.starts_with("mux_runtime-") && name.ends_with(".lib"))
                            })
                })
                .collect::<Vec<_>>();
            candidates.sort();
            if let Some(path) = candidates.into_iter().next() {
                return path;
            }
        }
    }
    panic!("build mux-runtime or set MUX_RUNTIME_LIB before running compiler tests");
}
