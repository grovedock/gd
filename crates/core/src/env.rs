//! Settings from the environment under the names used before 4.0.

/// Copies every `PANDO_X` variable to `GD_X` unless `GD_X` is set, so 3.x environments keep
/// working, and says so once. Call first thing in `main`, before any thread starts.
// ponytail: drop in 5.0, with the PANDO_ names.
pub fn adopt_legacy() {
    let old: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("PANDO_")).collect();
    for (k, v) in &old {
        let new = format!("GD_{}", &k["PANDO_".len()..]);
        if std::env::var_os(&new).is_none() {
            // SAFETY: called before any other thread exists, so nothing reads the environment
            // concurrently.
            unsafe { std::env::set_var(&new, v) };
        }
    }
    if let Some((k, _)) = old.first() {
        eprintln!("warning: {k} and other PANDO_* variables are deprecated; rename them to GD_*");
    }
}
