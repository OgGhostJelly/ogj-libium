pub mod filters;
pub mod structs;

use std::{
    fs::{create_dir_all, File},
    io::{BufReader, Result},
    path::{Path, PathBuf},
    sync::LazyLock,
};

pub static DEFAULT_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    crate::HOME
        .join(".config")
        .join("ferium")
        .join("config.json")
});

/// Open the config file at `path` and deserialise it into a config struct
pub fn read_config(path: impl AsRef<Path>) -> Result<structs::Config> {
    if !path.as_ref().exists() {
        create_dir_all(path.as_ref().parent().expect("Invalid config directory"))?;
        write_config(&path, &structs::Config::default())?;
    }

    let config_file = BufReader::new(File::open(&path)?);
    let config: structs::Config = serde_json::from_reader(config_file)?;

    Ok(config)
}

pub fn read_profile(path: impl AsRef<Path>) -> Result<Option<structs::Profile>> {
    let file = match File::open(&path) {
        Ok(file) => file,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let profile_file = BufReader::new(file);
    let mut profile: structs::Profile = serde_json::from_reader(profile_file)?;
    
    profile.backwards_compat();

    profile.mods.sort_unstable_by_key(|mod_| mod_.name.to_lowercase());

    Ok(Some(profile))
}

/// Serialise `config` and write it to the config file at `path`
pub fn write_config(path: impl AsRef<Path>, config: &structs::Config) -> Result<()> {
    let config_file = File::create(path)?;
    serde_json::to_writer_pretty(config_file, config)?;
    Ok(())
}

/// Serialise `profile` and write it to the profile file at `path`
pub fn write_profile(path: impl AsRef<Path>, profile: &structs::Profile) -> Result<()> {
    let profile_file = File::create(&path)?;
    serde_json::to_writer_pretty(profile_file, profile)?;
    Ok(())
}