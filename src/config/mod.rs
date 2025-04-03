mod legacy;
pub mod structs;

use std::{
    collections::HashMap,
    fs::{self, create_dir_all},
    io::Result,
    path::{Path, PathBuf},
    sync::LazyLock,
};

use thiserror::Error;

pub static DEFAULT_CONFIG_PATH: LazyLock<PathBuf> = LazyLock::new(|| {
    crate::HOME
        .join(".config")
        .join("ferium")
        .join("ogj-config.toml")
});

/// Open the config file at `path` and deserialise it into a config struct
pub fn read_config(path: impl AsRef<Path>) -> Result<structs::Config> {
    if !path.as_ref().exists() {
        create_dir_all(path.as_ref().parent().expect("Invalid config directory"))?;
        write_config(&path, &structs::Config::default())?;
    }

    let contents = fs::read_to_string(&path)?;
    let config: structs::Config = toml::from_str(&contents).map_err(invalid_data_to_io)?;

    Ok(config)
}

fn invalid_data_to_io<E>(error: E) -> std::io::Error
where
    E: Into<Box<dyn std::error::Error + Send + Sync>>,
{
    std::io::Error::new(std::io::ErrorKind::InvalidData, error)
}

pub fn read_profile(path: impl AsRef<Path>) -> Result<Option<structs::Profile>> {
    let file = match fs::read_to_string(path) {
        Ok(file) => file,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound) => return Ok(None),
        Err(e) => return Err(e.into()),
    };

    let profile: structs::Profile = toml::from_str(&file).map_err(invalid_data_to_io)?;

    Ok(Some(profile))
}

/// Serialise `config` and write it to the config file at `path`
pub fn write_config(path: impl AsRef<Path>, config: &structs::Config) -> Result<()> {
    let contents = toml::to_string(config).map_err(invalid_data_to_io)?;
    fs::write(path, contents)?;
    Ok(())
}

/// Serialise `profile` and write it to the profile file at `path`
pub fn write_profile(path: impl AsRef<Path>, profile: &structs::Profile) -> Result<()> {
    let contents = toml::to_string(profile).map_err(invalid_data_to_io)?;
    fs::write(path, contents)?;
    Ok(())
}

pub fn migrate_legacy_config(
    path: impl AsRef<Path>,
) -> std::result::Result<structs::Config, MigrateError> {
    let empty: &Path = Path::new("");

    let dir = path.as_ref().parent().unwrap_or(empty);
    let config = legacy::read_config(path.as_ref())?;

    let mut profiles = vec![];

    for legacy_profile in config.profiles {
        let legacy::structs::Profile {
            name,
            output_dir: mods_dir,
            filters,
            mods: legacy_mods,
            ..
        } = legacy_profile;

        let path = {
            let mut path = dir.join(&name);
            path.set_extension("toml");
            path
        };

        let profile = structs::Profile {
            filters: legacy::migrate_filters(filters)?,
            mods: {
                let mut mods = HashMap::new();
                for mod_ in legacy_mods {
                    let id = mod_.slug.clone().unwrap_or(mod_.name.clone());
                    mods.insert(id, mod_.try_into()?);
                }
                mods
            },
            shaders: HashMap::new(),
            modpacks: HashMap::new(),
            resourcepacks: HashMap::new(),
        };

        write_profile(&path, &profile)?;

        let shaderpacks_dir = mods_dir.parent().unwrap_or(empty).join("shaderpacks");

        let resourcepacks_dir = mods_dir.parent().unwrap_or(empty).join("resourcepacks");

        let item = structs::ProfileItem {
            path,
            name,
            mods_dir,
            shaderpacks_dir,
            resourcepacks_dir,
        };

        profiles.push(item);
    }

    let config = structs::Config {
        active_profile: config.active_profile,
        profiles,
        active_modpack: config.active_modpack,
        modpacks: config
            .modpacks
            .into_iter()
            .map(|modpack| modpack.into())
            .collect(),
    };

    Ok(config)
}

#[derive(Error, Debug)]
#[error(transparent)]
pub enum MigrateError {
    Semver(#[from] semver::Error),
    Regex(#[from] regex::Error),
    IO(#[from] std::io::Error),
}
