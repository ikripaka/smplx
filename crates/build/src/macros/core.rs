use std::path::{Path, PathBuf};

use proc_macro2::Span;
use quote::quote;

use simplicityhl::ast::ElementsJetHinter;
use simplicityhl::{AbiMeta, RawSources, TemplateAst, UnstableFeatures};

use super::codegen::{
    GeneratedArgumentTokens, GeneratedProgramTraitHelperTokens, GeneratedWitnessTokens, SimfContractMeta,
    convert_contract_name_to_contract_module,
};
use super::parse::{SimfContent, SynFilePath};

pub fn expand(input: &SynFilePath) -> syn::Result<proc_macro2::TokenStream> {
    let simf_content = SimfContent::new(input)?;
    let raw_files = declared_raw_files(&simf_content)?;
    let abi_meta = compile_simf(&simf_content, &raw_files)?;
    let generated = expand_inner(simf_content, abi_meta, &raw_files)?;

    Ok(generated)
}

fn expand_inner(
    simf_content: SimfContent,
    meta: AbiMeta,
    raw_files: &[(String, PathBuf)],
) -> syn::Result<proc_macro2::TokenStream> {
    let mod_ident = convert_contract_name_to_contract_module(&simf_content.contract_name);

    let derived_meta = SimfContractMeta::try_from(simf_content, meta)?;

    let program_helpers = construct_program_helpers(&derived_meta, raw_files)?;
    let witness_helpers = construct_witness_helpers(&derived_meta)?;
    let arguments_helpers = construct_argument_helpers(&derived_meta)?;
    let program_trait_helpers = construct_program_trait_helpers(&derived_meta)?;

    Ok(quote! {
        pub mod #mod_ident{
            #program_helpers

            #witness_helpers

            #arguments_helpers

            #program_trait_helpers
        }
    })
}

fn construct_program_helpers(
    derived_meta: &SimfContractMeta,
    raw_files: &[(String, PathBuf)],
) -> syn::Result<proc_macro2::TokenStream> {
    let contract_content = &derived_meta.simf_content.content;
    let contract_source_name = &derived_meta.contract_source_const_name;

    let raw_sources_name = &derived_meta.contract_raw_sources_const_name;
    let raw_paths = raw_files.iter().map(|(path, _)| path);

    let raw_files = raw_files
        .iter()
        .map(|(_, file)| {
            file.to_str()
                .map(str::to_owned)
                .ok_or_else(|| call_site_error(format!("raw Simplicity path is not UTF-8: {}", file.display())))
        })
        .collect::<Result<Vec<_>, _>>()?;

    Ok(quote! {
        pub const #contract_source_name: &str = #contract_content;

        /// The raw Simplicity files of the contract's `extern fn` declarations, by declared path.
        pub const #raw_sources_name: &[(&str, &str)] = &[#((#raw_paths, include_str!(#raw_files))),*];
    })
}

/// The raw Simplicity files of the contract's `extern fn` declarations: their declared paths
/// (keys), and where `simplex build` mirrored them beside the contract.
fn declared_raw_files(content: &SimfContent) -> syn::Result<Vec<(String, PathBuf)>> {
    let raw_root = content.path.parent().map(Path::to_path_buf).unwrap_or_default();
    let keys = TemplateAst::declared_raw_paths(&content.content, &UnstableFeatures::all())
        .map_err(|diags| call_site_error(diags.render_to_string()))?;

    Ok(keys
        .into_iter()
        .map(|key| (key.to_string(), raw_root.join(key.as_ref())))
        .collect())
}

fn compile_simf(content: &SimfContent, raw_files: &[(String, PathBuf)]) -> syn::Result<AbiMeta> {
    let mut raw_sources = RawSources::new();
    for (key, file) in raw_files {
        let text = std::fs::read_to_string(file)
            .map_err(|e| call_site_error(format!("cannot read raw Simplicity file '{}': {e}", file.display())))?;
        raw_sources.insert(key.as_str(), text);
    }

    TemplateAst::new_with_raw_sources(
        content.content.as_str(),
        &UnstableFeatures::all(),
        Box::new(ElementsJetHinter),
        &raw_sources,
    )
    .map_err(|diags| call_site_error(diags.render_to_string()))?
    .generate_abi_meta()
    .map_err(call_site_error)
}

fn call_site_error(message: impl std::fmt::Display) -> syn::Error {
    syn::Error::new(Span::call_site(), message)
}

fn construct_witness_helpers(derived_meta: &SimfContractMeta) -> syn::Result<proc_macro2::TokenStream> {
    let GeneratedWitnessTokens {
        imports,
        struct_token_stream,
        struct_impl,
    } = derived_meta.witness_struct.generate_witness_impl()?;

    Ok(quote! {
        pub use build_witness::*;
        mod build_witness {
            #imports

            #struct_token_stream

            #struct_impl
        }
    })
}

fn construct_argument_helpers(derived_meta: &SimfContractMeta) -> syn::Result<proc_macro2::TokenStream> {
    let GeneratedArgumentTokens {
        imports,
        struct_token_stream,
        struct_impl,
    } = derived_meta.args_struct.generate_arguments_impl()?;

    Ok(quote! {
        pub use build_arguments::*;
        mod build_arguments {
            #imports

            #struct_token_stream

            #struct_impl
        }
    })
}

fn construct_program_trait_helpers(derived_meta: &SimfContractMeta) -> syn::Result<proc_macro2::TokenStream> {
    let GeneratedProgramTraitHelperTokens { imports, helper_impls } =
        derived_meta.generate_program_trait_helpers_impl()?;

    Ok(quote! {
        mod program_helpers {
            #imports

            #helper_impls
        }
    })
}
