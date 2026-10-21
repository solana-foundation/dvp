use {
    codama::Codama,
    std::{env, fs, path::Path},
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=src/");
    println!("cargo:rerun-if-env-changed=GENERATE_IDL");

    generate_idl().map_err(|e| format!("Failed to generate IDL: {e}").into())
}

fn generate_idl() -> Result<(), Box<dyn std::error::Error>> {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR")?;
    let crate_path = Path::new(&manifest_dir);
    let codama = Codama::load(crate_path)?;
    let idl_json = codama.get_json_idl()?;

    let mut parsed: serde_json::Value = serde_json::from_str(&idl_json)?;
    // Codama 0.7 applies #[codama(name)] to definitions, but type links still
    // use the Rust identifier. Keep refund arguments linked to the renamed type.
    for instruction in parsed["program"]["instructions"].as_array_mut().unwrap() {
        for argument in instruction["arguments"].as_array_mut().unwrap() {
            let ty = &mut argument["type"];
            if ty["kind"] == "definedTypeLinkNode" && ty["name"] == "legBrefund" {
                ty["name"] = "legBRefund".into();
            }
        }
    }
    let mut formatted_json = serde_json::to_string_pretty(&parsed)?;
    formatted_json.push('\n');

    let project_root = Path::new(&manifest_dir).parent().unwrap();
    let idl_dir = project_root.join("idl");
    fs::create_dir_all(&idl_dir)?;
    let idl_path = idl_dir.join("dvp_swap_program.json");
    fs::write(&idl_path, formatted_json)?;

    println!("cargo:warning=IDL written to: {}", idl_path.display());
    Ok(())
}
