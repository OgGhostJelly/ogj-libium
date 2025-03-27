use crate::add;

use derive_more::derive::Display;
use serde::{de::Visitor, Deserialize, Serialize};
use std::{collections::HashMap, env::current_dir, fmt, path::PathBuf, str::FromStr};

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct Config {
    #[serde(skip_serializing_if = "is_zero")]
    #[serde(default)]
    pub active_profile: usize,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub profiles: Vec<ProfileItem>,

    #[serde(skip_serializing_if = "is_zero")]
    #[serde(default)]
    pub active_modpack: usize,

    #[serde(skip_serializing_if = "Vec::is_empty")]
    #[serde(default)]
    pub modpacks: Vec<Modpack>,
}

#[derive(Deserialize, Serialize, Debug, Default, Clone)]
pub struct ProfileItem {
    /// The path to the profile `.json` file.
    pub path: PathBuf,
    /// The unique name of the profile.
    pub name: String,
    /// The directory to download mod files to
    pub output_dir: PathBuf,
}

impl ProfileItem {
    pub fn infer_path(name: String, output_dir: PathBuf) -> std::io::Result<Self> {
        let mut path = current_dir()?.join(&name);
        path.set_extension("json");
        Ok(Self {
            path,
            name,
            output_dir,
        })
    }
}

const fn is_zero(n: &usize) -> bool {
    *n == 0
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Modpack {
    pub name: String,
    pub output_dir: PathBuf,
    pub install_overrides: bool,
    pub identifier: ModpackIdentifier,
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum ModpackIdentifier {
    CurseForgeModpack(i32),
    ModrinthModpack(String),
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Profile {
    #[serde(flatten)]
    pub filters: Filters,
    #[serde(default)]
    pub mods: HashMap<String, Source>,
    #[serde(default)]
    pub shaders: HashMap<String, Source>,
    #[serde(default)]
    pub modpacks: HashMap<String, Source>,
    #[serde(default)]
    pub resourcepacks: HashMap<String, Source>,
}

impl Profile {
    /// A simple contructor that automatically deals with converting to filters
    pub fn new(versions: Vec<Version>, mod_loader: ModLoader) -> Self {
        Self {
            filters: Filters {
                versions,
                mod_loaders: match mod_loader {
                    ModLoader::Fabric | ModLoader::Quilt => {
                        vec![ModLoader::Fabric, ModLoader::Quilt]
                    }
                    mod_loader => vec![mod_loader],
                },
            },
            mods: HashMap::new(),
            shaders: HashMap::new(),
            modpacks: HashMap::new(),
            resourcepacks: HashMap::new(),
        }
    }

    pub fn push_mod(&mut self, id: String, source: Source) -> Result<(), add::Error> {
        if self.mods.contains_key(&id) {
            return Err(add::Error::AlreadyAdded);
        }
        self.mods.insert(id, source);
        Ok(())
    }

    pub fn mod_ids(&self) -> impl Iterator<Item = &SourceId> {
        self.mods.iter().flat_map(|(_, source)| source.ids())
    }
}

#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub enum ModIdentifier {
    CurseForgeProject(i32),
    ModrinthProject(String),
    GitHubRepository(String, String),

    PinnedCurseForgeProject(i32, i32),
    PinnedModrinthProject(String, String),
    PinnedGitHubRepository((String, String), i32),
}

impl ModIdentifier {
    pub fn to_source_id(self) -> SourceId {
        match self {
            ModIdentifier::CurseForgeProject(id) => SourceId::Curseforge(id),
            ModIdentifier::ModrinthProject(id) => SourceId::Modrinth(id),
            ModIdentifier::GitHubRepository(owner, repo) => SourceId::Github(owner, repo),
            _ => todo!(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
pub enum Source {
    Single(SourceId),
    Multiple(Vec<Source>),
    Detailed {
        #[serde(flatten)]
        filters: Filters,
        src: Box<Source>,
    },
}

impl Source {
    pub fn filters(&self) -> Option<&Filters> {
        match self {
            Source::Single(_) => None,
            Source::Multiple(_) => None,
            Source::Detailed { filters, .. } => Some(filters),
        }
    }

    pub fn ids(&self) -> SourceIdsIter<'_> {
        SourceIdsIter { srcs: vec![self] }
    }

    pub fn each_sources<'a>(
        &'a self,
        filters: Vec<&'a Filters>,
        mut f: impl FnMut(Vec<&'a Filters>, &'a SourceId),
    ) -> impl FnMut(Vec<&'a Filters>, &'a SourceId) {
        match self {
            Source::Single(source_id) => f(filters, source_id),
            Source::Multiple(sources) => {
                for source in sources {
                    f = source.each_sources(filters.clone(), f);
                }
            }
            Source::Detailed {
                filters: new_filter,
                src,
            } => {
                let mut filters = filters;
                filters.push(new_filter);
                f = src.each_sources(filters, f);
            }
        }
        f
    }

    pub fn github(owner: String, repo: String, filters: Option<Filters>) -> Self {
        Self::from_id(SourceId::Github(owner, repo), filters)
    }

    pub fn curseforge(id: i32, filters: Option<Filters>) -> Self {
        Self::from_id(SourceId::Curseforge(id), filters)
    }

    pub fn modrinth(id: String, filters: Option<Filters>) -> Self {
        Self::from_id(SourceId::Modrinth(id), filters)
    }

    pub fn from_id(source_id: SourceId, filters: Option<Filters>) -> Self {
        let source = Self::Single(source_id);

        match filters {
            Some(filters) => Self::Detailed {
                filters,
                src: Box::new(source),
            },
            None => source,
        }
    }
}

#[derive(Debug, Clone)]
pub enum SourceId {
    Curseforge(i32),
    Modrinth(String),
    Github(String, String),
}

impl SourceId {
    pub fn to_string(&self) -> String {
        match self {
            SourceId::Curseforge(id) => format!("cf:{id}"),
            SourceId::Modrinth(id) => format!("mr:{id}"),
            SourceId::Github(owner, repo) => format!("gh:{owner}/{repo}"),
        }
    }
}

impl Serialize for SourceId {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for SourceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(SourceTagVisitor)
    }
}

struct SourceTagVisitor;

impl<'de> Visitor<'de> for SourceTagVisitor {
    type Value = SourceId;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "a source tag e.g \"modrinth:abc\" or \"cf:123\"")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let Some(index) = v.find(':') else {
            return Err(E::custom(format!(
                "missing `:` separator in source tag {v:?}"
            )));
        };

        let (tag, id) = v.split_at(index);
        let id = &id[1..];

        match tag {
            "cf" | "curseforge" => Ok(SourceId::Curseforge(match id.parse() {
                Ok(value) => value,
                Err(e) => return Err(E::custom(e)),
            })),
            "mr" | "modrinth" => Ok(SourceId::Modrinth(id.to_string())),
            "gh" | "github" => {
                let Some(index) = id.find('/') else {
                    return Err(E::custom(format!(
                        "missing `/` separator in github source {tag}:{id}"
                    )));
                };

                let (owner, repo) = id.split_at(index);
                let repo = &repo[1..];

                Ok(SourceId::Github(owner.into(), repo.into()))
            }
            _ => Err(E::unknown_variant(
                tag,
                &["mr", "modrinth", "cf", "curseforge", "gh", "github"],
            )),
        }
    }
}

/// Helper to recursively iterate through every id in a source.
pub struct SourceIdsIter<'a> {
    srcs: Vec<&'a Source>,
}

impl<'a> SourceIdsIter<'a> {
    fn next_source(&mut self, source: &'a Source) -> Option<&'a SourceId> {
        match source {
            Source::Single(id) => Some(id),
            Source::Multiple(sources) => {
                for source in sources.iter().rev() {
                    self.srcs.push(source);
                }
                self.next()
            }
            Source::Detailed { src, .. } => self.next_source(&src),
        }
    }
}

impl<'a> Iterator for SourceIdsIter<'a> {
    type Item = &'a SourceId;

    fn next(&mut self) -> Option<Self::Item> {
        let value = self.srcs.pop()?;
        self.next_source(value)
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Filters {
    #[serde(default, alias = "version", with = "serde_version")]
    pub versions: Vec<Version>,
    #[serde(default)]
    pub mod_loaders: Vec<ModLoader>,
}

mod serde_version {
    use serde::{Deserialize, Serialize};

    use super::Version;

    type T = Vec<Version>;

    pub fn serialize<S>(data: &T, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer
    {
        if data.len() == 1 {
            data[0].serialize(serializer)
        } else {
            data.serialize(serializer)
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<T, D::Error>
    where
        D: serde::Deserializer<'de>
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum VersionList {
            Single(Version),
            Multiple(Vec<Version>)
        }

        match VersionList::deserialize(deserializer)? {
            VersionList::Single(version) => Ok(vec![version]),
            VersionList::Multiple(versions) => Ok(versions),
        }
    }
}

impl Filters {
    pub fn concat(self, other: Filters) -> Filters {
        Filters {
            versions: [self.versions, other.versions].concat(),
            mod_loaders: [self.mod_loaders, other.mod_loaders].concat(),
        }
    }

    pub fn matches(&self, version: &str, mod_loader: &ModLoader) -> bool {
        self.version_matches(version) && self.mod_loader_matches(mod_loader)
    }

    pub fn mod_loader_matches(&self, mod_loader: &ModLoader) -> bool {
        self.mod_loaders.iter().any(|p| p == mod_loader)
    }

    pub fn version_matches(&self, version: &str) -> bool {
        self.versions.iter().any(|p| p.matches(version))
    }
}

#[derive(Debug, Clone)]
pub struct Version(glob::Pattern);

impl Version {
    pub fn matches(&self, s: &str) -> bool {
        self.0.matches(s)
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0.as_str())
    }
}

impl FromStr for Version {
    type Err = glob::PatternError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(glob::Pattern::new(s)?))
    }
}

impl Serialize for Version {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.0.as_str())
    }
}

impl<'de> Deserialize<'de> for Version {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(VersionVisitor)
    }
}

struct VersionVisitor;

impl<'de> Visitor<'de> for VersionVisitor {
    type Value = Version;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "a unix-style glob pattern")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        match v.parse() {
            Ok(value) => Ok(value),
            Err(e) => Err(E::custom(e)),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Display, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ModLoader {
    Quilt,
    Fabric,
    Forge,
    #[clap(name = "neoforge")]
    NeoForge,
}

#[derive(thiserror::Error, Debug, PartialEq, Eq)]
#[error("The given string is not a recognised mod loader")]
pub struct ModLoaderParseError;

impl FromStr for ModLoader {
    type Err = ModLoaderParseError;

    // This implementation is case-insensitive
    fn from_str(from: &str) -> Result<Self, Self::Err> {
        match from.trim().to_lowercase().as_str() {
            "quilt" => Ok(Self::Quilt),
            "fabric" => Ok(Self::Fabric),
            "forge" => Ok(Self::Forge),
            "neoforge" => Ok(Self::NeoForge),
            _ => Err(Self::Err {}),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Display, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ReleaseChannel {
    Release,
    Beta,
    Alpha,
}
