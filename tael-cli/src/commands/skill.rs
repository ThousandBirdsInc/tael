use std::env;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};

const SKILL_MD: &str = include_str!("../../SKILL.md");
const SKILL_NAME: &str = "tael";

fn resolve_dir(project: bool) -> Result<PathBuf> {
    if project {
        let cwd = env::current_dir().context("failed to read current directory")?;
        Ok(cwd.join(".claude").join("skills").join(SKILL_NAME))
    } else {
        let home = env::var("HOME").context("HOME is not set; cannot locate ~/.claude")?;
        Ok(PathBuf::from(home)
            .join(".claude")
            .join("skills")
            .join(SKILL_NAME))
    }
}

pub fn install(project: bool, force: bool) -> Result<()> {
    let dir = resolve_dir(project)?;
    let target = dir.join("SKILL.md");
    let existed_before = target.exists();

    if existed_before && !force {
        bail!(
            "{} already exists — pass --force to overwrite",
            target.display()
        );
    }

    fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create {}", dir.display()))?;
    fs::write(&target, SKILL_MD)
        .with_context(|| format!("failed to write {}", target.display()))?;

    let action = if existed_before { "Updated" } else { "Installed" };
    let scope = if project { "project" } else { "personal" };
    println!("{action} tael skill ({scope}) at {}", target.display());

    if !existed_before {
        println!();
        println!(
            "Claude Code picks up new skill directories on startup — restart any \
             running Claude Code session for the skill to become available."
        );
    }

    Ok(())
}

pub fn print_path(project: bool) -> Result<()> {
    let target = resolve_dir(project)?.join("SKILL.md");
    println!("{}", target.display());
    Ok(())
}
