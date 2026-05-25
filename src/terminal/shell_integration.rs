use std::fs;
use std::path::PathBuf;

const BASH_RCFILE: &str = include_str!("../../assets/dsterm-integration.bashrc");
const ZSH_ZSHRC: &str = include_str!("../../assets/dsterm-integration.zshrc");
const FISH_CONFIG: &str = include_str!("../../assets/dsterm-integration.fish");

pub struct IntegrationPaths {
    pub dir: PathBuf,
    pub bashrc: PathBuf,
    pub zshrc_dir: PathBuf,
    pub fish_config: PathBuf,
}

pub fn write_integration_files(session_uuid: &str) -> std::io::Result<IntegrationPaths> {
    let mut dir = std::env::temp_dir();
    dir.push(format!("dsterm-integration-{session_uuid}"));
    fs::create_dir_all(&dir)?;

    let bashrc = dir.join("bashrc");
    fs::write(&bashrc, BASH_RCFILE)?;

    let zshrc_dir = dir.join("zsh");
    fs::create_dir_all(&zshrc_dir)?;
    fs::write(zshrc_dir.join(".zshrc"), ZSH_ZSHRC)?;

    let fish_config = dir.join("config.fish");
    fs::write(&fish_config, FISH_CONFIG)?;

    Ok(IntegrationPaths {
        dir,
        bashrc,
        zshrc_dir,
        fish_config,
    })
}

pub fn integration_command(paths: &IntegrationPaths) -> (String, Vec<String>) {
    let shell = std::env::var("SHELL").unwrap_or_else(|_| {
        String::from("/data/data/com.termux/files/usr/bin/bash")
    });
    let base = shell.rsplit('/').next().unwrap_or("bash");
    match base {
        "zsh" => {
            (shell.clone(), vec!["-i".to_string()])
        }
        "fish" => {
            (
                shell.clone(),
                vec![
                    "-C".to_string(),
                    format!("source {}", paths.fish_config.display()),
                    "-i".to_string(),
                ],
            )
        }
        _ => {
            (
                shell.clone(),
                vec![
                    "--rcfile".to_string(),
                    paths.bashrc.display().to_string(),
                    "-i".to_string(),
                ],
            )
        }
    }
}
