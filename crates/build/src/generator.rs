use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::io::{BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use serde::Serialize;

use simplicityhl::RawSources;
use simplicityhl::TemplateAst;
use simplicityhl::UnstableFeatures;
use simplicityhl::ast::ElementsJetHinter;
use simplicityhl::resolution::DependencyMap;
use simplicityhl::resolution::ValidatedDeps;
use simplicityhl::source::CanonPath;
use simplicityhl::source::CanonSourceFile;

use crate::contract_id::ContractId;
use crate::macros::codegen::{
    construct_program_name, convert_contract_name_to_contract_module,
    convert_contract_name_to_contract_raw_sources_const, convert_contract_name_to_contract_source_const,
};
use crate::macros::parse::SimfContent;
use crate::resolver::ArtifactsResolver;

use super::error::BuildError;

const METADATA_FILENAME: &str = "metadata.json";

pub struct ArtifactsGenerator {}

/// A single processed (flattened) `.simf` file with all metadata needed for binding generation.
///
/// Created once per source file and carries everything downstream — no recomputation
/// of paths or contract names in later stages.
struct SimfArtifact {
    /// Path relative to `base_dir` (e.g. `hash/func/sha256.simf`).
    /// Used to mirror the source structure under `out_dir/simf/`.
    relative_path: PathBuf,
    /// Full path to the file written under `out_dir/simf/`.
    /// Passed directly to `include_simf!` — no path reconstruction needed.
    mirrored_path: PathBuf,
    /// Contract name extracted from the `.simf` source file.
    contract_name: String,
}

#[derive(Default)]
struct TreeNode {
    files: Vec<SimfArtifact>,
    dirs: HashMap<String, TreeNode>,
}

/// A raw Simplicity (`.simpl`) file of the project.
struct RawFile {
    /// Path relative to `base_dir`, with `/` separators; mirrored under `out_dir/simf/`.
    relative_path: String,
    text: Arc<str>,
}

#[derive(Default, Serialize)]
struct Metadata {
    sources: Sources,
}

/// The sources of the artifacts, by path relative to `base_dir`.
#[derive(Default, Serialize)]
struct Sources {
    simplicityhl: BTreeMap<String, SourceEntry>,
    simplicity: BTreeMap<String, RawEntry>,
}

#[derive(Serialize)]
struct SourceEntry {
    cmr: ContractId,
    content: String,
}

/// A `.simpl` file, copied beside the flattened contracts.
#[derive(Serialize)]
struct RawEntry {
    /// SHA-256 of the file.
    sha256: String,
    /// The entries contracts import from it, with their CMRs.
    entries: BTreeMap<String, String>,
}

impl ArtifactsGenerator {
    pub fn generate_artifacts(
        root_dir: &Path,
        out_dir: impl AsRef<Path>,
        base_dir: impl AsRef<Path>,
        simfs: &[impl AsRef<Path>],
        simpls: &[impl AsRef<Path>],
        validated_deps: &ValidatedDeps,
    ) -> Result<(), BuildError> {
        let out_dir = out_dir.as_ref();
        let base_dir = base_dir.as_ref();
        let raw_files = Self::read_raw_files(simpls, base_dir)?;

        let json_metadata_file = out_dir.join(METADATA_FILENAME);

        let pathdiff =
            pathdiff::diff_paths(base_dir, root_dir).ok_or_else(|| BuildError::FailedToFindCorrectRelativePath {
                root_dir: root_dir.to_path_buf(),
                simf_file: base_dir.to_path_buf(),
            })?;

        let simf_out_dir = out_dir.join(pathdiff);
        let mut metadata = Metadata::default();

        for simf in simfs {
            Self::check_module_names(simf.as_ref(), base_dir)?;
        }

        let artifacts = simfs
            .iter()
            .map(|s| {
                Self::process_simf(
                    s.as_ref(),
                    base_dir,
                    validated_deps,
                    &raw_files,
                    &simf_out_dir,
                    &mut metadata,
                )
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Every contract compiled: mirror the raw files beside them, where their flattened
        // `extern fn` paths point.
        for raw in &raw_files {
            let target = simf_out_dir.join(&raw.relative_path);
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&target, raw.text.as_bytes())?;
            metadata
                .sources
                .simplicity
                .entry(raw.relative_path.clone())
                .or_insert_with(|| RawEntry {
                    sha256: sha256_hex(raw.text.as_bytes()),
                    entries: BTreeMap::new(),
                });
        }

        let file = std::fs::File::create(&json_metadata_file)?;
        serde_json::to_writer_pretty(BufWriter::new(file), &metadata)?;

        let tree = Self::build_tree(artifacts)?;

        Self::generate_bindings(root_dir, out_dir, tree)?;

        Ok(())
    }

    pub fn build_dependency_map(
        validated_deps: &ValidatedDeps,
        entry_root_dir: impl AsRef<Path>,
    ) -> Result<DependencyMap, BuildError> {
        let canon_entry_root =
            CanonPath::canonicalize(entry_root_dir.as_ref()).map_err(BuildError::PathCanonicalization)?;

        validated_deps
            .with_root(canon_entry_root)
            .map_err(|e| BuildError::DependencyMap(e.to_string()))
    }

    /// Processes a single `.simf` source file into its metadata:
    /// - Writes its content to the mirrored path under `simf_out_dir`
    /// - Extracts the contract name
    ///
    /// All path and name information needed for downstream stages is captured
    /// here so later steps never need to re-derive anything.
    fn process_simf(
        source: &Path,
        base_dir: &Path,
        validated_deps: &ValidatedDeps,
        raw_files: &[RawFile],
        simf_out_dir: &Path,
        metadata: &mut Metadata,
    ) -> Result<SimfArtifact, BuildError> {
        let relative_path = source
            .strip_prefix(base_dir)
            .map_err(BuildError::NoBasePathForGeneration)?
            .to_path_buf();

        let mirrored_path = simf_out_dir.join(&relative_path);

        if let Some(parent) = mirrored_path.parent() {
            fs::create_dir_all(parent)?;
        }

        // The compiler is given the raw files by key: relative to this contract's directory.
        let contract_dir = relative_path.parent().unwrap_or(Path::new(""));
        let raw_sources = Self::raw_sources_for(contract_dir, raw_files)?;
        let (source_entry, raw_imports) = Self::process_content(source, base_dir, validated_deps, &raw_sources)?;
        // The compiler reads a raw file that is not given from disk, but only the given ones are
        // mirrored into the artifacts: a contract must not import any other.
        if let Some(import) = raw_imports
            .iter()
            .find(|import| raw_sources.get(&import.path).is_none())
        {
            return Err(BuildError::GenerationFailed(format!(
                "the raw Simplicity file `{}` was not given: add it to `simpl_files`",
                import.path
            )));
        }
        fs::write(&mirrored_path, &source_entry.content)?;

        for import in raw_imports {
            // The import's key is relative to the contract; the metadata's, to `base_dir`.
            let path = to_key(&ArtifactsResolver::normalize(&contract_dir.join(import.path.as_ref())))?;
            if let Some(raw) = raw_files.iter().find(|raw| raw.relative_path == path) {
                metadata
                    .sources
                    .simplicity
                    .entry(path)
                    .or_insert_with(|| RawEntry {
                        sha256: sha256_hex(raw.text.as_bytes()),
                        entries: BTreeMap::new(),
                    })
                    .entries
                    .insert(import.entry.to_string(), import.cmr.to_string());
            }
        }

        let relative_path_str = relative_path.display().to_string();
        metadata.sources.simplicityhl.insert(relative_path_str, source_entry);

        let contract_name = SimfContent::extract_content_from_path(&source.to_path_buf())
            .map_err(BuildError::FailedToExtractContent)?
            .contract_name;

        Ok(SimfArtifact {
            relative_path,
            mirrored_path,
            contract_name,
        })
    }

    /// Reads the project's `.simpl` files, by path relative to `base_dir`.
    fn read_raw_files(simpls: &[impl AsRef<Path>], base_dir: &Path) -> Result<Vec<RawFile>, BuildError> {
        let canon_base = base_dir.canonicalize().unwrap_or_else(|_| base_dir.to_path_buf());

        simpls
            .iter()
            .map(|path| {
                let path = path.as_ref();
                let relative = path
                    .strip_prefix(base_dir)
                    .or_else(|_| path.strip_prefix(&canon_base))
                    .map_err(BuildError::NoBasePathForGeneration)?;
                Ok(RawFile {
                    relative_path: to_key(relative)?,
                    text: Arc::from(fs::read_to_string(path)?),
                })
            })
            .collect()
    }

    /// The raw files keyed for a contract in `contract_dir` (relative to `base_dir`): by their
    /// path relative to that directory, as its `extern fn` declarations resolve them.
    fn raw_sources_for(contract_dir: &Path, raw_files: &[RawFile]) -> Result<RawSources, BuildError> {
        let mut sources = RawSources::new();

        for raw in raw_files {
            let key = pathdiff::diff_paths(Path::new(&raw.relative_path), contract_dir).ok_or_else(|| {
                BuildError::FailedToFindCorrectRelativePath {
                    root_dir: contract_dir.to_path_buf(),
                    simf_file: PathBuf::from(&raw.relative_path),
                }
            })?;
            sources.insert(to_key(&key)?, Arc::clone(&raw.text));
        }

        Ok(sources)
    }

    /// Reads and processes the content of a `.simf` file.
    fn process_content(
        source: &Path,
        project_root: &Path,
        validated_deps: &ValidatedDeps,
        raw_sources: &RawSources,
    ) -> Result<(SourceEntry, Vec<simplicityhl::RawImport>), BuildError> {
        let canon_source = CanonPath::canonicalize(source).map_err(BuildError::PathCanonicalization)?;
        let content = fs::read_to_string(source)?;
        let canon_source_file = CanonSourceFile::new(canon_source, Arc::from(content));
        let dependency_map = Self::build_dependency_map(validated_deps, project_root)?;

        let template = TemplateAst::new_with_dep_and_raw_sources(
            canon_source_file.clone(),
            &dependency_map,
            &UnstableFeatures::all(),
            Box::new(ElementsJetHinter),
            raw_sources,
        )
        .map_err(|diags| BuildError::CompilerDiagnostics(Box::new(diags)))?;
        let flattened = TemplateAst::flatten(canon_source_file, &dependency_map, &UnstableFeatures::all())
            .map_err(|diags| BuildError::CompilerDiagnostics(Box::new(diags)))?;

        let entry = SourceEntry {
            cmr: ContractId::from_template(&template)?,
            content: flattened,
        };

        Ok((entry, template.raw_imports().to_vec()))
    }

    /// Every directory between `base_dir` and `source` becomes a module of the bindings.
    fn check_module_names(source: &Path, base_dir: &Path) -> Result<(), BuildError> {
        let relative_path = source
            .strip_prefix(base_dir)
            .map_err(BuildError::NoBasePathForGeneration)?;

        for dir in relative_path.parent().into_iter().flat_map(Path::components) {
            let Component::Normal(name) = dir else {
                continue;
            };
            let name = name.to_string_lossy();

            if syn::parse_str::<syn::Ident>(&name).is_err() {
                return Err(BuildError::InvalidModuleName {
                    name: name.into_owned(),
                    path: source.to_path_buf(),
                });
            }
        }

        Ok(())
    }

    /// Arranges a flat list of artifacts into a tree mirroring the source directory layout.
    fn build_tree(artifacts: Vec<SimfArtifact>) -> Result<TreeNode, BuildError> {
        let mut root = TreeNode::default();

        for artifact in artifacts {
            let components: Vec<_> = artifact
                .relative_path
                .components()
                .filter_map(|c| {
                    if let Component::Normal(name) = c {
                        Some(name.to_string_lossy().into_owned())
                    } else {
                        None
                    }
                })
                .collect();

            // All components except the last are directories; the last is the file itself
            let mut current = &mut root;
            for dir in &components[..components.len().saturating_sub(1)] {
                current = current.dirs.entry(dir.clone()).or_default();
            }

            current.files.push(artifact);
        }

        Ok(root)
    }

    /// Recursively generates bindings for every node in the tree.
    fn generate_bindings(root_dir: &Path, out_dir: &Path, tree: TreeNode) -> Result<(), BuildError> {
        fs::create_dir_all(out_dir)?;

        let mut mod_names = Vec::new();

        for artifact in tree.files {
            let mod_name = Self::generate_simf_binding(root_dir, out_dir, artifact)?;
            mod_names.push(mod_name);
        }

        for (dir_name, subtree) in tree.dirs {
            Self::generate_bindings(root_dir, &out_dir.join(&dir_name), subtree)?;
            mod_names.push(dir_name);
        }

        Self::generate_mod_rs(out_dir, &mod_names)?;

        Ok(())
    }

    /// Generates a single `.rs` binding file for one simf artifact.
    fn generate_simf_binding(root_dir: &Path, out_dir: &Path, artifact: SimfArtifact) -> Result<String, BuildError> {
        let output_file = out_dir.join(format!("{}.rs", artifact.contract_name));

        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_file)?;

        let pathdiff = pathdiff::diff_paths(&artifact.mirrored_path, root_dir).ok_or_else(|| {
            BuildError::FailedToFindCorrectRelativePath {
                root_dir: root_dir.to_path_buf(),
                simf_file: artifact.mirrored_path.clone(),
            }
        })?;

        let code = Self::generate_simf_binding_code(&artifact.contract_name, &pathdiff)?;

        Self::expand_file(code, &mut file)?;

        Ok(artifact.contract_name)
    }

    fn generate_mod_rs(out_dir: impl AsRef<Path>, mod_names: &[String]) -> Result<(), BuildError> {
        let output_file = out_dir.as_ref().join("mod.rs");
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&output_file)?;

        let code = Self::generate_mod_binding_code(mod_names)?;

        Self::expand_file(code, &mut file)?;

        Ok(())
    }

    fn expand_file(code: TokenStream, buf: &mut dyn Write) -> Result<(), BuildError> {
        let file: syn::File = syn::parse2(code).map_err(|e| BuildError::GenerationFailed(e.to_string()))?;
        let prettystr = prettyplease::unparse(&file);

        buf.write_all(b"// This file is @generated by Simplex. Do not edit manually.\n\n")?;
        buf.write_all(prettystr.as_bytes())?;
        buf.flush()?;

        Ok(())
    }

    fn generate_simf_binding_code(contract_name: &str, target_simf: &Path) -> Result<TokenStream, BuildError> {
        let program_name = construct_program_name(contract_name);
        let include_simf_source_const = convert_contract_name_to_contract_source_const(contract_name);
        let include_simf_module = convert_contract_name_to_contract_module(contract_name);
        let include_simf_raw_sources_const = convert_contract_name_to_contract_raw_sources_const(contract_name);
        let target_simf_str = target_simf.to_string_lossy().into_owned();

        let code = quote! {
            use simplex::include_simf;
            use simplex::program::{Program};
            use simplex::provider::SimplicityNetwork;
            use simplex::simplicityhl::elements::Script;
            use simplex::simplicityhl::elements::secp256k1_zkp::XOnlyPublicKey;
            use simplex::simplicityhl::Arguments;

            #[derive(Clone)]
            pub struct #program_name {
                program: Program,
            }

            // TODO: refactor | bad design
            impl #program_name {
                pub const SOURCE: &'static str = #include_simf_module::#include_simf_source_const;
                /// The raw Simplicity files of the contract's `extern fn` declarations, by declared path.
                pub const RAW_SOURCES: &'static [(&'static str, &'static str)] =
                    #include_simf_module::#include_simf_raw_sources_const;

                #[must_use]
                pub fn new(arguments: impl Into<Arguments>) -> Self {
                    Self {
                        program: Program::new(Self::SOURCE, arguments.into())
                            .with_raw_sources(Self::RAW_SOURCES.iter().copied()),
                    }
                }

                #[must_use]
                pub fn with_taproot_pubkey(mut self, pub_key: XOnlyPublicKey) -> Self {
                    self.program = self.program.with_taproot_pubkey(pub_key);
                    self
                }

                #[must_use]
                pub fn with_storage_capacity(mut self, capacity: usize) -> Self {
                    self.program = self.program.with_storage_capacity(capacity);
                    self
                }

                // No #[must-use], as we don't return any value.
                pub fn set_storage_at(&mut self, index: usize, new_value: impl Into<Vec<u8>>) {
                    self.program.set_storage_at(index, new_value);
                }

                #[must_use]
                pub fn get_storage_len(&self) -> usize {
                    self.program.get_storage_len()
                }

                #[must_use]
                pub fn get_storage(&self) -> &[Vec<u8>] {
                    self.program.get_storage()
                }

                #[must_use]
                pub fn get_storage_at(&self, index: usize) -> Vec<u8> {
                    self.program.get_storage_at(index)
                }

                #[must_use]
                pub fn get_script_pubkey(&self, network: &SimplicityNetwork) -> Script {
                    self.program.get_script_pubkey(network)
                }

                #[must_use]
                pub fn get_script_hash(&self, network: &SimplicityNetwork) -> [u8; 32] {
                    self.program.get_script_hash(network)
                }

                #[must_use]
                pub fn get_cmr(&self) -> [u8; 32] {
                    self.program.get_cmr()
                }

                #[must_use]
                pub fn get_tapleaf_hash(&self) -> [u8; 32] {
                    self.program.get_tapleaf_hash()
                }
            }

            impl AsRef<Program> for #program_name {
                fn as_ref(&self) -> &Program {
                    &self.program
                }
            }

            impl AsMut<Program> for #program_name {
                fn as_mut(&mut self) -> &mut Program {
                    &mut self.program
                }
            }

            include_simf!(#target_simf_str);
        };

        Ok(code)
    }

    fn generate_mod_binding_code(mod_names: &[String]) -> Result<TokenStream, BuildError> {
        let mod_idents = mod_names.iter().map(|x| format_ident!("{x}")).collect::<Vec<_>>();

        let code = quote! {
            #![allow(clippy::all)]
            #(
                #[rustfmt::skip]
                pub mod #mod_idents;
            )*
        };

        Ok(code)
    }
}

/// A relative path as a raw-source key: `/` separators.
fn to_key(path: &Path) -> Result<String, BuildError> {
    let parts = path
        .components()
        .map(|c| match c {
            Component::Normal(name) => name.to_str().map(str::to_owned),
            Component::ParentDir => Some("..".to_owned()),
            Component::CurDir => Some(".".to_owned()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| BuildError::GenerationPath(format!("not a relative UTF-8 path: '{}'", path.display())))?;
    Ok(parts.join("/"))
}

// TODO: change on the CMR
fn sha256_hex(bytes: &[u8]) -> String {
    use simplicityhl::simplicity::hashes::sha256;
    sha256::Hash::hash(bytes).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    // TODO: refactor
    #[test]
    fn raw_diagnostics_are_preserved_before_artifacts_are_written() {
        use simplicityhl::error::{Error, Location};
        use simplicityhl::resolution::DependencyMapBuilder;

        let root = std::env::temp_dir().join(format!("simplex-build-raw-{}", std::process::id()));
        let src = root.join("simf");
        let out = root.join("out");

        fs::create_dir_all(src.join("nested")).unwrap();
        fs::create_dir_all(&out).unwrap();
        fs::write(
            src.join("main.simf"),
            "use crate::nested::lib::eq; fn main() { assert!(eq(1, 1)); }",
        )
        .unwrap();
        fs::write(
            src.join("nested/lib.simf"),
            "#[path = \"eq.simpl\"] pub extern fn eq(a: u32, b: u32) -> u32;",
        )
        .unwrap();
        fs::write(src.join("nested/eq.simpl"), "export := jet_eq_32 : (2^32 * 2^32) -> 2").unwrap();

        let deps = DependencyMapBuilder::new().validate_deps().unwrap();
        let build = || {
            ArtifactsGenerator::generate_artifacts(
                &root,
                &out,
                &src,
                &[src.join("main.simf")],
                &[src.join("nested/eq.simpl")],
                &deps,
            )
        };
        let error = build().unwrap_err();
        let rendered = error.to_string();

        assert!(
            std::error::Error::source(&error)
                .and_then(|source| source.downcast_ref::<Box<simplicityhl::error::DiagnosticManager>>())
                .is_some()
        );

        let BuildError::CompilerDiagnostics(diags) = error else {
            panic!("unexpected error");
        };

        let diag = diags
            .diagnostics()
            .iter()
            .find(|d| matches!(d.error(), Error::SimplicityTypeMismatch { .. }))
            .unwrap();
        let Location::Code(span) = diag.location() else {
            panic!("missing declaration span");
        };
        assert_eq!(
            diags.sources().unwrap().path(span.file_id).unwrap().as_path(),
            src.join("nested/lib.simf").canonicalize().unwrap()
        );

        for part in ["export", "nested/lib.simf", "expected structural", "found"] {
            assert!(rendered.contains(part), "{rendered}");
        }
        assert!(!out.join("metadata.json").exists());
        assert!(!out.join("simf/main.simf").exists());

        fs::write(
            src.join("nested/lib.simf"),
            "#[path = \"eq.simpl\"] pub extern fn eq(a: u32, b: u32) -> bool;",
        )
        .unwrap();

        build().unwrap();

        assert!(out.join("metadata.json").exists());
        // The raw file is mirrored beside the contract, and the flattened source names it relatively.
        assert!(out.join("simf/nested/eq.simpl").exists());

        let flattened = fs::read_to_string(out.join("simf/main.simf")).unwrap();
        assert!(flattened.contains("path = \"nested/eq.simpl\""), "{flattened}");

        let metadata: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(out.join("metadata.json")).unwrap()).unwrap();
        assert!(metadata["sources"]["simplicityhl"]["main.simf"]["cmr"].is_string());
        assert!(metadata["sources"]["simplicity"]["nested/eq.simpl"]["entries"]["export"].is_string());

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn directories_must_be_rust_identifiers() {
        let base = Path::new("/project/simf");

        for valid in ["vault.simf", "vaults/vault.simf", "vaults/_2024/vault.simf"] {
            assert!(
                ArtifactsGenerator::check_module_names(&base.join(valid), base).is_ok(),
                "{valid}"
            );
        }
        for invalid in ["my-contracts/vault.simf", "2024/vault.simf", "mod/vault.simf"] {
            assert!(
                matches!(
                    ArtifactsGenerator::check_module_names(&base.join(invalid), base),
                    Err(BuildError::InvalidModuleName { .. })
                ),
                "{invalid}"
            );
        }
    }
}
