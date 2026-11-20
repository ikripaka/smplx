#[cfg(unix)]
#[test]
fn simplex_test_propagates_failed_test_runner_status() -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    use smplx_cli::config::CONFIG_FILENAME;

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let test_dir = std::env::temp_dir().join(format!("smplx-cli-exit-status-{suffix}"));
    let bin_dir = test_dir.join("bin");
    fs::create_dir_all(&bin_dir)?;
    fs::write(test_dir.join(CONFIG_FILENAME), "")?;

    let nextest = bin_dir.join("smplx-nextest");
    fs::write(&nextest, "#!/bin/sh\nexit 7\n")?;
    let mut permissions = fs::metadata(&nextest)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(&nextest, permissions)?;

    let mut path_entries = vec![bin_dir];
    if let Some(path) = std::env::var_os("PATH") {
        path_entries.extend(std::env::split_paths(&path));
    }

    let output = Command::new(env!("CARGO_BIN_EXE_simplex"))
        .arg("test")
        .current_dir(&test_dir)
        .env("PATH", std::env::join_paths(path_entries)?)
        .output()?;

    assert_ne!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("test command failed with exit status 7")
            || stderr.contains("test command failed with exit status 7"),
        "stdout: {stdout}\nstderr: {stderr}"
    );

    fs::remove_dir_all(test_dir)?;
    Ok(())
}

// TODO: refactor
#[test]
fn build_reports_raw_type_mismatch_and_accepts_fixed_contract() {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };
    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("simplex-cli-raw-{suffix}"));
    let src = root.join("simf");

    fs::create_dir_all(&src).unwrap();
    fs::write(root.join("Simplex.toml"), "").unwrap();
    fs::write(src.join("equal.simpl"), "export := jet_eq_32 : (2^32 * 2^32) -> 2").unwrap();

    let make_source = |ret| {
        format!(
            "#[path = \"equal.simpl\"] extern fn eq(a: u32, b: u32) -> {ret}; fn main() {{ assert!(eq(witness::A, witness::B)); }}"
        )
    };

    fs::write(src.join("main.simf"), make_source("u32")).unwrap();

    let run = || {
        Command::new(env!("CARGO_BIN_EXE_simplex"))
            .arg("build")
            .current_dir(&root)
            .output()
            .unwrap()
    };
    let output = run();
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(!output.status.success(), "{text}");

    for part in ["equal", "main.simf", "expected structural", "found"] {
        assert!(text.contains(part), "{text}");
    }

    assert!(!root.join("src/artifacts/metadata.json").exists());

    fs::write(src.join("main.simf"), make_source("bool")).unwrap();

    let output = run();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(root.join("src/artifacts/metadata.json").exists());

    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();

    // `[patch]` applies only to the workspace being built: give the check crate the workspace's.
    let manifest = fs::read_to_string(workspace.join("Cargo.toml")).unwrap();
    let patch = manifest
        .split_once("[patch.crates-io]")
        .map(|(_, rest)| format!("[patch.crates-io]{}\n", rest.split("\n[").next().unwrap_or_default()))
        .unwrap_or_default();

    fs::write(root.join("Cargo.toml"), format!("[package]\nname = \"raw-binding-check\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[dependencies]\nsimplex = {{ package = \"smplx-std\", path = {:?}, default-features = false }}\n{patch}", workspace.join("crates/simplex"))).unwrap();
    fs::write(root.join("src/lib.rs"), r#"
        pub mod artifacts;
        #[cfg(test)] mod tests {
            use super::artifacts::main::{MainProgram, derived_main::MainWitness};
            use simplex::simplicityhl::{Arguments, WitnessValues};
            #[test] fn equality() {
                let program = MainProgram::new(Arguments::default());
                for (a,b,ok) in [(42,42,true), (42,43,false)] {
                    let witness = MainWitness { a, b };
                    let values: WitnessValues = (&witness).into();
                    let satisfied = program.as_ref().load().unwrap().satisfy(values).unwrap();
                    let mut machine = simplex::simplicityhl::simplicity::BitMachine::for_program(satisfied.redeem()).unwrap();
                    assert_eq!(machine.exec(satisfied.redeem(), &simplex::simplicityhl::dummy_env::dummy()).is_ok(), ok);
                }
            }
        }
    "#).unwrap();

    let flattened = root.join("src/artifacts/simf/main.simf");
    let valid = fs::read_to_string(&flattened).unwrap();

    fs::write(&flattened, valid.replace("-> bool", "-> u32")).unwrap();

    let cargo = |verb| {
        Command::new(env!("CARGO"))
            .arg(verb)
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", root.join("target"))
            .output()
            .unwrap()
    };
    let failure = cargo("check");
    let stderr = String::from_utf8_lossy(&failure.stderr);

    assert!(!failure.status.success(), "{stderr}");

    for part in ["equal", "expected structural", "found"] {
        assert!(stderr.contains(part), "{stderr}");
    }
    fs::write(&flattened, valid).unwrap();

    let success = cargo("test");
    assert!(success.status.success(), "{}", String::from_utf8_lossy(&success.stderr));

    fs::remove_dir_all(root).unwrap();
}

#[test]
fn build_gives_simpl_files_to_the_compiler_and_exports_them() {
    use std::{
        fs,
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("simplex-cli-simpl-{suffix}"));
    let src = root.join("simf");

    fs::create_dir_all(src.join("contracts")).unwrap();
    fs::create_dir_all(src.join("raw")).unwrap();
    fs::write(src.join("raw/equal.simpl"), "export := jet_eq_32 : (2^32 * 2^32) -> 2").unwrap();
    fs::write(
        src.join("contracts/main.simf"),
        "#[path = \"../raw/equal.simpl\"] extern fn eq(a: u32, b: u32) -> bool; \
         fn main() { assert!(eq(witness::A, witness::B)); }",
    )
    .unwrap();

    let build = |config: &str| {
        fs::write(root.join("Simplex.toml"), config).unwrap();
        Command::new(env!("CARGO_BIN_EXE_simplex"))
            .arg("build")
            .current_dir(&root)
            .output()
            .unwrap()
    };

    // A raw file left out of `simpl_files` is not given to the compiler, nor mirrored: rejected.
    let output = build("[build]\nsimpl_files = []\n");
    let text = String::from_utf8_lossy(&output.stderr).into_owned() + &String::from_utf8_lossy(&output.stdout);
    assert!(!output.status.success(), "{text}");
    // The compiler names the file by its key: relative to the contract.
    assert!(text.contains("`../raw/equal.simpl` was not given"), "{text}");

    let output = build("");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let artifacts = root.join("src/artifacts");

    // The raw file is mirrored like the sources, and the flattened contract names it relatively.
    assert_eq!(
        fs::read_to_string(artifacts.join("simf/raw/equal.simpl")).unwrap(),
        fs::read_to_string(src.join("raw/equal.simpl")).unwrap()
    );

    let flattened = fs::read_to_string(artifacts.join("simf/contracts/main.simf")).unwrap();
    assert!(flattened.contains("path = \"../raw/equal.simpl\""), "{flattened}");

    let metadata = fs::read_to_string(artifacts.join("metadata.json")).unwrap();
    assert!(!metadata.contains(root.to_str().unwrap()), "{metadata}");

    let metadata: serde_json::Value = serde_json::from_str(&metadata).unwrap();
    assert!(metadata["sources"]["simplicityhl"]["contracts/main.simf"]["cmr"].is_string());

    let raw = &metadata["sources"]["simplicity"]["raw/equal.simpl"];
    assert_eq!(raw["sha256"].as_str().unwrap().len(), 64);
    assert!(raw["entries"]["export"].is_string());

    fs::remove_dir_all(root).unwrap();
}
