//! Informational commands: games, status, verify, detect, identify.

use std::path::PathBuf;
use std::process;

use cnc_content::GameId;

pub fn cmd_games() {
    println!("Supported games:\n");
    for &game in GameId::ALL {
        let slug = game.slug();
        let title = game.title();
        let legal = match game {
            GameId::RedAlert => "Freeware (EA, 2008) — downloadable",
            GameId::TiberianDawn => "Freeware (EA, 2007) — downloadable",
            GameId::Dune2 => "NOT freeware — local source only",
            GameId::Dune2000 => "NOT freeware — local source only",
        };
        println!("  {slug:<10} {title}");
        println!("             Status: {legal}");
        println!();
    }
    println!("Freeware games:     cnc-content --game <slug> download");
    println!("Non-freeware games: cnc-content --game <slug> install <path>");
}

pub fn cmd_seed_config(policy: Option<&str>) {
    use cnc_content::SeedingPolicy;

    if let Some(name) = policy {
        let policy = SeedingPolicy::from_str_loose(name).unwrap_or_else(|| {
            eprintln!("Unknown seeding policy: '{name}'");
            eprintln!("Available: pause, always, keep, delete");
            process::exit(1);
        });

        let mut config = cnc_content::config::Config::load();
        config.seeding_policy = policy;
        if let Err(e) = config.save() {
            eprintln!("Warning: could not save config: {e}");
        }

        println!("Seeding policy set to: {}", policy.label());
        println!();
        println!(
            "  Allows seeding:    {}",
            if policy.allows_seeding() { "yes" } else { "no" }
        );
        println!(
            "  Retains archives:  {}",
            if policy.retains_archives() {
                "yes"
            } else {
                "no"
            }
        );
        println!();
        match policy {
            SeedingPolicy::PauseDuringOnlinePlay => {
                println!("Downloads will be shared with other players when idle.");
                println!("Seeding automatically pauses during online gameplay.");
            }
            SeedingPolicy::SeedAlways => {
                println!("Downloads will be shared continuously, even during online play.");
                println!("Recommended for users with high bandwidth.");
            }
            SeedingPolicy::KeepNoSeed => {
                println!("Downloaded archives are kept for fast re-extraction.");
                println!("No data is uploaded to other players.");
            }
            SeedingPolicy::ExtractAndDelete => {
                println!("Downloaded archives are deleted after extraction.");
                println!("Minimizes disk usage. Re-download required for repairs.");
            }
        }
    } else {
        let config = cnc_content::config::Config::load();
        println!("Current seeding policy: {}", config.seeding_policy.label());
        println!();
        println!("Available policies:");
        for (slug, policy) in [
            ("pause", SeedingPolicy::PauseDuringOnlinePlay),
            ("always", SeedingPolicy::SeedAlways),
            ("keep", SeedingPolicy::KeepNoSeed),
            ("delete", SeedingPolicy::ExtractAndDelete),
        ] {
            let marker = if policy == config.seeding_policy {
                " (current)"
            } else {
                ""
            };
            println!("  {slug:<10} {}{}", policy.label(), marker);
        }
        println!();
        println!("Set with: cnc-content seed-config <policy>");
        println!("Or per-download: cnc-content download --seed <policy>");
    }
}

pub fn cmd_status(content_root: &std::path::Path, game: GameId) {
    println!("{}", game.title());
    println!("Content directory: {}", content_root.display());
    println!();

    let game_packages = cnc_content::packages_for_game(game);
    let required: Vec<_> = game_packages.iter().filter(|p| p.required).collect();
    let optional: Vec<_> = game_packages.iter().filter(|p| !p.required).collect();

    if !required.is_empty() {
        println!("  Required:");
        for pkg in &required {
            let installed = pkg.test_files.iter().all(|f| content_root.join(f).exists());
            let marker = if installed { "✓" } else { "✗" };
            println!("    {marker} {}", pkg.title);

            if !installed {
                let missing: Vec<_> = pkg
                    .test_files
                    .iter()
                    .filter(|f| !content_root.join(f).exists())
                    .collect();
                if missing.len() <= 3 {
                    for f in &missing {
                        println!("        missing: {f}");
                    }
                } else {
                    println!("        missing: {} files", missing.len());
                }
            }
        }
    }

    if !optional.is_empty() {
        println!();
        println!("  Optional:");
        for pkg in &optional {
            let installed = pkg.test_files.iter().all(|f| content_root.join(f).exists());
            let marker = if installed { "✓" } else { "–" };
            let dl_hint = if !installed {
                match pkg.download {
                    Some(_) => " (downloadable)",
                    None => " (requires local source)",
                }
            } else {
                ""
            };
            println!("    {marker} {}{dl_hint}", pkg.title);
        }
    }

    println!();
    if cnc_content::is_content_complete(content_root, game) {
        println!("All required content is installed.");
        let optional_missing = cnc_content::missing_packages(content_root, game)
            .iter()
            .filter(|p| !p.required)
            .count();
        if optional_missing > 0 {
            println!(
                "{optional_missing} optional package(s) available. Run `cnc-content --game {} download --all` to fetch them.",
                game.slug()
            );
        }
    } else {
        let missing = cnc_content::missing_required_packages(content_root, game);
        println!(
            "{} required package(s) missing. Run `cnc-content --game {} download` to fetch them.",
            missing.len(),
            game.slug()
        );
    }
}

pub fn cmd_verify(content_root: &std::path::Path, game: GameId) {
    if !content_root.exists() {
        eprintln!(
            "Content directory does not exist: {}",
            content_root.display()
        );
        process::exit(1);
    }

    // Verify all installed packages for this game.
    let installed_ids: Vec<cnc_content::PackageId> = cnc_content::packages_for_game(game)
        .iter()
        .filter(|p| p.test_files.iter().all(|f| content_root.join(f).exists()))
        .map(|p| p.id)
        .collect();

    if installed_ids.is_empty() {
        println!("No {} content packages are installed.", game.title());
        return;
    }

    println!("Generating content manifest...");
    match cnc_content::verify::generate_manifest(content_root, game.slug(), "v1", &installed_ids) {
        Ok(manifest) => {
            println!("Verifying {} files...", manifest.files.len());
            let failures = cnc_content::verify::verify_installed_content(content_root, &manifest);
            if failures.is_empty() {
                println!("All {} files verified successfully.", manifest.files.len());
            } else {
                eprintln!("{} file(s) failed verification:", failures.len());
                for f in &failures {
                    eprintln!("  ✗ {f}");
                }
                process::exit(1);
            }
        }
        Err(e) => {
            eprintln!("Failed to generate manifest: {e}");
            process::exit(1);
        }
    }
}

pub fn cmd_detect() {
    println!("Scanning for local C&C content sources...\n");

    let detected = cnc_content::source::detect_all();

    if detected.is_empty() {
        println!("No local content sources detected.");
        println!();
        println!("Checked:");
        println!("  - Steam libraries");
        println!("  - Origin / EA App installs");
        println!("  - GOG.com / GOG Galaxy installs");
        println!("  - Windows registry (legacy Westwood/EA)");
        println!("  - OpenRA content directories");
        println!("  - Mounted disc volumes");
        println!();
        println!("Use `cnc-content download` to fetch content from mirrors instead.");
        return;
    }

    println!("Found {} source(s):\n", detected.len());
    for (i, src) in detected.iter().enumerate() {
        let source_def = cnc_content::source(src.source_id).unwrap_or_else(|| {
            eprintln!(
                "Internal error: no source definition for {:?}",
                src.source_id
            );
            std::process::exit(1);
        });
        println!(
            "  {}. {} ({:?})",
            i + 1,
            source_def.title,
            source_def.source_type
        );
        println!("     Path: {}", src.path.display());
        println!("     Provides {} package(s):", src.packages.len());
        for pkg_id in &src.packages {
            let pkg = cnc_content::package(*pkg_id).unwrap_or_else(|| {
                eprintln!("Internal error: no package definition for {pkg_id:?}");
                std::process::exit(1);
            });
            let req = if pkg.required { " (required)" } else { "" };
            println!("       - {}{req}", pkg.title);
        }
        println!();
    }

    println!("To install from a source:");
    println!("  cnc-content install <path>");
    println!("  cnc-content install <path> --package music");
}

pub fn cmd_identify(path: &std::path::Path) {
    if !path.exists() {
        eprintln!("Path does not exist: {}", path.display());
        process::exit(1);
    }

    println!("Scanning: {}", path.display());

    match cnc_content::verify::identify_source(path) {
        Some(source_id) => {
            let src = cnc_content::source(source_id).unwrap_or_else(|| {
                eprintln!("Internal error: no source definition for {source_id:?}");
                std::process::exit(1);
            });
            println!("Identified: {} ({:?})", src.title, src.source_type);

            // Show which packages this source provides.
            let provides: Vec<_> = cnc_content::packages::ALL_PACKAGES
                .iter()
                .filter(|p| p.sources.contains(&source_id))
                .collect();
            println!("Provides {} package(s):", provides.len());
            for pkg in &provides {
                let req = if pkg.required { " (required)" } else { "" };
                println!("  - {}{req}", pkg.title);
            }

            // Show available recipes.
            let recipes = cnc_content::recipes_for_source(source_id);
            if !recipes.is_empty() {
                println!("Install recipes: {} available", recipes.len());
                println!(
                    "  Run `cnc-content install {}` to install from this source.",
                    path.display()
                );
            }
        }
        None => {
            println!("No known C&C content source identified at this path.");
            println!(
                "Checked against {} known sources.",
                cnc_content::sources::ALL_SOURCES.len()
            );
        }
    }
}

/// Recursively lists all files under a directory with their sizes.
pub fn walkdir(dir: &std::path::Path) -> std::io::Result<Vec<(PathBuf, u64)>> {
    let mut files = Vec::new();
    walk_recursive(dir, &mut files)?;
    Ok(files)
}

fn walk_recursive(dir: &std::path::Path, out: &mut Vec<(PathBuf, u64)>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let ft = entry.file_type()?;
        if ft.is_dir() {
            walk_recursive(&entry.path(), out)?;
        } else if ft.is_file() {
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push((entry.path(), size));
        }
    }
    Ok(())
}

pub fn format_size(bytes: u64) -> String {
    if bytes >= 1_000_000_000 {
        format!("{:.1} GB", bytes as f64 / 1_073_741_824.0)
    } else if bytes >= 1_000_000 {
        format!("{:.1} MB", bytes as f64 / 1_048_576.0)
    } else if bytes >= 1000 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}
