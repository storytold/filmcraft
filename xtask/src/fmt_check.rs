use serde_json::Value;
use std::process::Command;

const MAX_PACKAGES: usize = 4096;
const MAX_ID_BYTES: usize = 16384;
const MAX_NAME_BYTES: usize = 1024;

fn array<'a>(md: &'a Value, field: &str) -> Result<&'a [Value], String> {
    let values = md.get(field).and_then(Value::as_array).ok_or_else(|| format!("format metadata requires an array: {field}"))?;
    if values.is_empty() || values.len() > MAX_PACKAGES {
        return Err(format!("format metadata {field} must contain 1..={MAX_PACKAGES} entries"));
    }
    Ok(values)
}

fn text<'a>(value: &'a Value, field: &str, limit: usize) -> Result<&'a str, String> {
    let text = value.get(field).and_then(Value::as_str).ok_or_else(|| format!("format metadata requires a string: {field}"))?;
    if text.trim().is_empty() || text.len() > limit {
        return Err(format!("format metadata {field} is blank or exceeds {limit} bytes"));
    }
    Ok(text)
}

fn workspace_package_names(md: &Value) -> Result<Vec<&str>, String> {
    let packages = array(md, "packages")?;
    let members = array(md, "workspace_members")?;
    let mut ids = Vec::new();
    ids.try_reserve(packages.len()).map_err(|_| "format package ID allocation failed")?;
    for package in packages {
        ids.push(text(package, "id", MAX_ID_BYTES)?);
    }
    ids.sort_unstable();
    if ids.windows(2).any(|pair| pair.first() == pair.last()) {
        return Err("format metadata contains duplicate package IDs".into());
    }
    let mut seen = Vec::new();
    seen.try_reserve(members.len()).map_err(|_| "format member ID allocation failed")?;
    let mut names = Vec::new();
    names.try_reserve(members.len()).map_err(|_| "format package name allocation failed")?;
    for member in members {
        let id = member
            .as_str()
            .filter(|id| !id.trim().is_empty() && id.len() <= MAX_ID_BYTES)
            .ok_or_else(|| "format metadata contains an invalid workspace member ID".to_string())?;
        seen.push(id);
        let package = packages
            .iter()
            .find(|package| package.get("id").and_then(Value::as_str) == Some(id))
            .ok_or_else(|| "format metadata is missing a workspace package".to_string())?;
        names.push(text(package, "name", MAX_NAME_BYTES)?);
    }
    seen.sort_unstable();
    if seen.windows(2).any(|pair| pair.first() == pair.last()) {
        return Err("format metadata contains duplicate workspace member IDs".into());
    }
    names.sort_unstable();
    if names.windows(2).any(|pair| pair.first() == pair.last()) {
        return Err("format metadata contains ambiguous workspace package names".into());
    }
    Ok(names)
}

fn run_checks(md: Option<&Value>, mut run: impl FnMut(&mut Command) -> Result<(), String>) -> Result<(), String> {
    let cargo = env!("CARGO");
    let Some(md) = md else { return run(Command::new(cargo).args(["fmt", "--check"])) };
    let names = workspace_package_names(md)?;
    let root = text(md, "workspace_root", MAX_ID_BYTES)?;
    if !std::path::Path::new(root).is_absolute() {
        return Err("format metadata workspace_root must be absolute".into());
    }
    for name in &names {
        run(Command::new(cargo).args(["fmt", "--check", "-p", name]).current_dir(root)).map_err(|error| format!("format check for package {name}: {error}"))?;
    }
    eprintln!("format: ok ({} workspace packages)", names.len());
    Ok(())
}

pub(super) fn check() -> Result<(), String> {
    #[cfg(windows)]
    {
        run_checks(Some(&super::metadata()?), super::run)
    }
    #[cfg(not(windows))]
    {
        run_checks(None, super::run)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::ffi::OsStr;

    fn metadata() -> Value {
        json!({
            "workspace_root": std::env::temp_dir().join("Workspace With Spaces").join("filmcraft"),
            "workspace_members": ["xtask-id", "engine-id", "media-id"],
            "packages": [
                {"id": "media-id", "name": "filmcraft-media"},
                {"id": "foreign-id", "name": "foreign"},
                {"id": "xtask-id", "name": "xtask"},
                {"id": "engine-id", "name": "filmcraft-engine"}
            ]
        })
    }

    #[test]
    fn workspace_members_include_xtask_in_sorted_order() {
        assert_eq!(workspace_package_names(&metadata()).expect("names"), ["filmcraft-engine", "filmcraft-media", "xtask"]);
    }

    #[test]
    fn commands_preserve_args_and_workspace_root() {
        let mut checked = Vec::new();
        let md = metadata();
        let expected_root = std::path::Path::new(md["workspace_root"].as_str().expect("root"));
        run_checks(Some(&md), |command| {
            assert_eq!(command.get_program(), OsStr::new(env!("CARGO")));
            assert_eq!(command.get_current_dir(), Some(expected_root));
            let args: Vec<_> = command.get_args().map(|arg| arg.to_str().expect("text").to_owned()).collect();
            assert_eq!(args.get(..3), Some(["fmt".into(), "--check".into(), "-p".into()].as_slice()));
            assert_eq!(args.len(), 4);
            checked.push(args.last().expect("package").clone());
            Ok(())
        })
        .expect("all checks");
        assert_eq!(checked, ["filmcraft-engine", "filmcraft-media", "xtask"]);
    }

    #[test]
    fn runner_failure_stops_before_next_package() {
        for failure in ["launch failed", "nonzero formatter status"] {
            let mut calls = 0;
            let error = run_checks(Some(&metadata()), |_| {
                calls += 1;
                if calls == 2 { Err(failure.into()) } else { Ok(()) }
            })
            .expect_err("failed check");
            assert_eq!(calls, 2);
            assert!(error.contains("filmcraft-media"));
            assert!(error.contains(failure));
        }
    }

    #[test]
    fn non_windows_uses_original_single_command() {
        let mut calls = 0;
        run_checks(None, |command| {
            calls += 1;
            assert_eq!(command.get_program(), OsStr::new(env!("CARGO")));
            assert_eq!(command.get_current_dir(), None);
            assert_eq!(command.get_args().collect::<Vec<_>>(), [OsStr::new("fmt"), OsStr::new("--check")]);
            Ok(())
        })
        .expect("original check");
        assert_eq!(calls, 1);
    }

    #[test]
    fn malformed_metadata_never_runs_formatter() {
        let mut cases = vec![json!(null), json!({}), json!({"workspace_members": [], "packages": []})];
        for (field, values) in [
            (
                "workspace_members",
                vec![
                    json!(null),
                    json!({}),
                    json!([]),
                    json!([null]),
                    json!([""]),
                    json!([" "]),
                    json!(["missing"]),
                    json!(["xtask-id", "xtask-id"]),
                    json!(["x".repeat(16385)]),
                ],
            ),
            (
                "packages",
                vec![json!(null), json!({}), json!([]), json!([null]), json!([{"id": "xtask-id", "name": "xtask"}, {"id": "xtask-id", "name": "xtask"}])],
            ),
            ("workspace_root", vec![json!(null), json!(""), json!("relative/root"), json!("x".repeat(16385))]),
        ] {
            for value in values {
                let mut md = metadata();
                md[field] = value;
                cases.push(md);
            }
        }
        for field in ["id", "name"] {
            for value in [json!(null), json!(""), json!(" "), json!("x".repeat(16385))] {
                let mut md = metadata();
                md["packages"][2][field] = value;
                cases.push(md);
            }
        }
        let mut duplicate_names = metadata();
        duplicate_names["packages"][3]["name"] = json!("xtask");
        cases.push(duplicate_names);
        for field in ["workspace_members", "packages"] {
            let mut md = metadata();
            md[field] = Value::Array(vec![json!("over cap"); 4097]);
            cases.push(md);
        }
        for md in cases {
            let mut calls = 0;
            assert!(
                run_checks(Some(&md), |_| {
                    calls += 1;
                    Ok(())
                })
                .is_err(),
                "metadata unexpectedly accepted: {md}"
            );
            assert_eq!(calls, 0, "validation must finish before launching formatters");
        }
    }
}
