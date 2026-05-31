//! The `ferrobrew` binary: a thin dispatcher over the [`ferrobrew`] library.
//!
//! Commands are added here as their subsystems land. Anything not yet implemented exits with a
//! clear message rather than deferring to Ruby — ferrobrew is a standalone replacement, so missing
//! parity stays visible.

use std::process::ExitCode;

use ferrobrew::config::Config;
use ferrobrew::system::Tag;
use ferrobrew::{FerroError, Result};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("");
    let rest = &args[args.len().min(1)..];

    let result = match command {
        "config" => {
            print_config();
            Ok(())
        }
        "info" => info(rest.first()),
        "" => Err(FerroError::Other("no command given".into())),
        other => Err(FerroError::Unsupported(format!("command '{other}'"))),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("ferrobrew: {err}");
            ExitCode::FAILURE
        }
    }
}

/// Print the resolved configuration and platform tag — handy for verifying path derivation.
fn print_config() {
    let config = Config::from_env();
    let tag = Tag::current();
    println!("Bottle tag:          {}", tag.to_bottle_tag());
    println!("HOMEBREW_PREFIX:     {}", config.prefix.display());
    println!("HOMEBREW_REPOSITORY: {}", config.repository.display());
    println!("HOMEBREW_CELLAR:     {}", config.cellar.display());
    println!("HOMEBREW_CASKROOM:   {}", config.caskroom.display());
    println!("HOMEBREW_CACHE:      {}", config.cache.display());
    println!("HOMEBREW_LIBRARY:    {}", config.library.display());
}

/// Fetch a formula from the JSON API and print its description, version, deps and current bottle.
fn info(name: Option<&String>) -> Result<()> {
    let name = name.ok_or_else(|| FerroError::Other("usage: ferrobrew info <formula>".into()))?;
    let formula = ferrobrew::api::fetch_formula(name)?;
    let tag = Tag::current().to_bottle_tag();

    println!(
        "{}: {}",
        formula.full_name,
        formula.desc.as_deref().unwrap_or("(no description)")
    );
    if let Some(version) = formula.pkg_version() {
        println!("version: {version}");
    }
    if let Some(homepage) = &formula.homepage {
        println!("homepage: {homepage}");
    }
    if !formula.dependencies.is_empty() {
        println!("dependencies: {}", formula.dependencies.join(", "));
    }
    match formula.bottle.file_for_tag(&tag) {
        Some(bottle) => {
            println!("bottle ({tag}):");
            println!("  url:    {}", bottle.url);
            println!("  sha256: {}", bottle.sha256);
        }
        None => println!("no bottle available for {tag}"),
    }
    Ok(())
}
