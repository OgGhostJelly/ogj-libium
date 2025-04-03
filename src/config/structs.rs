use crate::add;

use derive_more::derive::Display;
use semver::Prerelease;
use serde::{de::Visitor, Deserialize, Serialize};
use std::{
    collections::{hash_map::Entry, HashMap},
    env::current_dir,
    fmt,
    fs::File,
    marker::PhantomData,
    path::PathBuf,
    str::FromStr,
};

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
    pub mods_dir: PathBuf,
    /// The directory to download mod files to
    pub shaderpacks_dir: PathBuf,
    /// The directory to download mod files to
    pub resourcepacks_dir: PathBuf,
}

impl ProfileItem {
    pub fn infer_path(
        path: Option<PathBuf>,
        name: String,
        mods_dir: PathBuf,
        resourcepacks_dir: PathBuf,
        shaderpacks_dir: PathBuf,
    ) -> std::io::Result<Self> {
        let path = match path {
            Some(path) => path,
            None => {
                let mut path = current_dir()?.join(&name);
                path.set_extension("toml");
                path
            }
        };

        let _ = File::create(&path)?;

        let path = path.canonicalize()?;

        Ok(Self {
            path,
            name,
            mods_dir,
            shaderpacks_dir,
            resourcepacks_dir,
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub mods: HashMap<String, Source>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub shaders: HashMap<String, Source>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub modpacks: HashMap<String, Source>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub resourcepacks: HashMap<String, Source>,
}

impl Profile {
    /// A simple contructor that automatically deals with converting to filters
    pub fn new(versions: Option<Vec<Version>>, mod_loader: ModLoader) -> Self {
        Self {
            filters: Filters {
                versions,
                mod_loaders: match mod_loader {
                    ModLoader::Fabric | ModLoader::Quilt => {
                        Some(vec![ModLoader::Fabric, ModLoader::Quilt])
                    }
                    mod_loader => Some(vec![mod_loader]),
                },
                ..Filters::empty()
            },
            mods: HashMap::new(),
            shaders: HashMap::new(),
            modpacks: HashMap::new(),
            resourcepacks: HashMap::new(),
        }
    }

    pub fn push_mod(&mut self, id: String, source: Source) -> Result<(), add::Error> {
        Self::push_map(&mut self.mods, id, source)
    }

    pub fn push_shader(&mut self, id: String, source: Source) -> Result<(), add::Error> {
        println!("{id} {source:?}");
        Self::push_map(&mut self.shaders, id, source)
    }

    pub fn push_modpack(&mut self, id: String, source: Source) -> Result<(), add::Error> {
        Self::push_map(&mut self.modpacks, id, source)
    }

    pub fn push_resourcepack(&mut self, id: String, source: Source) -> Result<(), add::Error> {
        Self::push_map(&mut self.resourcepacks, id, source)
    }

    pub fn push_map(
        map: &mut HashMap<String, Source>,
        id: String,
        source: Source,
    ) -> Result<(), add::Error> {
        for source_id in source.ids() {
            let has_duplicates = map
                .iter()
                .flat_map(|(_, source)| source.ids())
                .any(|mod_id| mod_id == source_id);
            if has_duplicates {
                return Err(add::Error::AlreadyAdded);
            }
        }

        match map.entry(id.clone()) {
            Entry::Occupied(e) => {
                let source = match e.remove() {
                    e @ Source::Single(_) | e @ Source::Detailed { .. } => {
                        Source::Multiple(vec![e, source])
                    }
                    Source::Multiple(mut sources) => {
                        sources.push(source);
                        Source::Multiple(sources)
                    }
                };

                if map.contains_key(&id) {
                    return Err(add::Error::AlreadyAdded);
                }

                map.insert(id.clone(), source);
            }
            Entry::Vacant(e) => {
                let _ = e.insert(source);
            }
        }

        Ok(())
    }

    pub fn mod_ids(&self) -> impl Iterator<Item = &SourceId> {
        self.mods.iter().flat_map(|(_, source)| source.ids())
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

    pub fn github(owner: String, repo: String, filters: Filters) -> Self {
        Self::from_id(SourceId::Github(owner, repo), filters)
    }

    pub fn curseforge(id: i32, filters: Filters) -> Self {
        Self::from_id(SourceId::Curseforge(id), filters)
    }

    pub fn modrinth(id: String, filters: Filters) -> Self {
        Self::from_id(SourceId::Modrinth(id), filters)
    }

    pub fn from_id(source_id: SourceId, filters: Filters) -> Self {
        let source = Self::Single(source_id);

        if filters.is_empty() {
            source
        } else {
            Self::Detailed {
                filters,
                src: Box::new(source),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceId {
    Curseforge(i32),
    Modrinth(String),
    Github(String, String),

    PinnedCurseforge(i32, i32),
    PinnedModrinth(String, String),
    PinnedGithub((String, String), i32),
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SourceId::Curseforge(id) => write!(f, "cf:{id}"),
            SourceId::Modrinth(id) => write!(f, "mr:{id}"),
            SourceId::Github(owner, repo) => write!(f, "gh:{owner}/{repo}"),
            SourceId::PinnedCurseforge(id, pin) => write!(f, "cf:{id}*{pin}"),
            SourceId::PinnedModrinth(id, pin) => write!(f, "mr:{id}*{pin}"),
            SourceId::PinnedGithub((owner, repo), pin) => write!(f, "gh:{owner}/{repo}*{pin}"),
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

        fn parse_with_pin<'inp, Id, Pin>(
            inp: &'inp str,
            f_id: impl Fn(&'inp str) -> Id,
            f_pin: impl Fn(&'inp str) -> Pin,
        ) -> (Id, Option<Pin>) {
            match inp.rfind('*') {
                Some(index) => {
                    let (id, pin) = inp.split_at(index);
                    (f_id(id), Some(f_pin(&pin[1..])))
                }
                None => (f_id(inp), None),
            }
        }

        match tag {
            "cf" | "curseforge" => match parse_with_pin(id, |id| id.parse(), |pin| pin.parse()) {
                (Ok(id), None) => Ok(SourceId::Curseforge(id)),
                (Ok(id), Some(Ok(pin))) => Ok(SourceId::PinnedCurseforge(id, pin)),
                (Err(e), _) | (_, Some(Err(e))) => Err(E::custom(e)),
            },
            "mr" | "modrinth" => match parse_with_pin(id, |id| id, |pin| pin) {
                (id, None) => Ok(SourceId::Modrinth(id.to_owned())),
                (id, Some(pin)) => Ok(SourceId::PinnedModrinth(id.to_owned(), pin.to_owned())),
            },
            "gh" | "github" => {
                let parsed = parse_with_pin(
                    id,
                    |id| {
                        let Some(index) = id.find('/') else {
                            return Err(E::custom(format!(
                                "missing `/` separator in github source {tag}:{id}"
                            )));
                        };

                        let (owner, repo) = id.split_at(index);
                        let repo = &repo[1..];

                        Ok((owner, repo))
                    },
                    |pin| pin.parse(),
                );

                match (parsed.0?, parsed.1) {
                    ((owner, repo), None) => {
                        Ok(SourceId::Github(owner.to_owned(), repo.to_owned()))
                    }
                    ((owner, repo), Some(Ok(pin))) => Ok(SourceId::PinnedGithub(
                        (owner.to_owned(), repo.to_owned()),
                        pin,
                    )),
                    (_, Some(Err(e))) => Err(E::custom(e)),
                }
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
            Source::Detailed { src, .. } => self.next_source(src),
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

#[derive(Debug, Clone, Copy)]
pub enum SourceKind {
    Mods,
    Resourcepacks,
    Shaders,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct Filters {
    #[serde(default, alias = "version", with = "MaybeListOrSingle")]
    pub versions: Option<Vec<Version>>,
    #[serde(default, alias = "mod_loader", with = "MaybeListOrSingle")]
    pub mod_loaders: Option<Vec<ModLoader>>,
    #[serde(default, alias = "release_channel")]
    pub release_channels: Option<Vec<ReleaseChannel>>,
    #[serde(default)]
    pub filename: Option<Regex>,
    #[serde(default)]
    pub title: Option<Regex>,
    #[serde(default)]
    pub description: Option<Regex>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum MaybeListOrSingle<T> {
    Single(T),
    Multiple(Vec<T>),
}

impl<T> MaybeListOrSingle<T> {
    pub fn serialize<S>(data: &Option<Vec<T>>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
        T: serde::Serialize,
    {
        match data {
            Some(data) => {
                if data.len() == 1 {
                    data[0].serialize(serializer)
                } else {
                    data.serialize(serializer)
                }
            }
            None => data.serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<Vec<T>>, D::Error>
    where
        D: serde::Deserializer<'de>,
        T: serde::Deserialize<'de>,
    {
        match <Self as serde::Deserialize>::deserialize(deserializer)? {
            MaybeListOrSingle::Single(item) => Ok(Some(vec![item])),
            MaybeListOrSingle::Multiple(items) => Ok(Some(items)),
        }
    }
}

impl Filters {
    pub fn empty() -> Filters {
        Filters::default()
    }

    pub fn concat(self, other: Filters) -> Filters {
        fn concat_opts<T: Clone>(a: Option<Vec<T>>, b: Option<Vec<T>>) -> Option<Vec<T>> {
            match (a, b) {
                (None, None) => None,
                (a, b) => Some([a.unwrap_or(vec![]), b.unwrap_or(vec![])].concat()),
            }
        }

        fn concat_regex(a: Option<Regex>, b: Option<Regex>) -> Option<Regex> {
            match (a, b) {
                (None, None) => None,
                (pat, None) | (None, pat) => pat,
                (Some(a), Some(b)) => Some(
                    format!("{}|{}", a.0, b.0)
                        .parse()
                        .expect("Joining regex expr with OR should always be a valid pattern"),
                ),
            }
        }

        Filters {
            versions: concat_opts(self.versions, other.versions),
            mod_loaders: concat_opts(self.mod_loaders, other.mod_loaders),
            release_channels: concat_opts(self.release_channels, other.release_channels),
            filename: concat_regex(self.filename, other.filename),
            title: concat_regex(self.title, other.title),
            description: concat_regex(self.description, other.description),
        }
    }

    pub fn release_channel_matches(&self, release_channel: &ReleaseChannel) -> bool {
        let Some(release_channels) = &self.release_channels else {
            return true;
        };

        release_channels.iter().any(|c| c == release_channel)
    }

    pub fn filename_matches(&self, filename: &str) -> bool {
        let Some(filename_pat) = &self.filename else {
            return true;
        };

        filename_pat.0.is_match(filename)
    }

    pub fn title_matches(&self, title: &str) -> bool {
        let Some(title_pat) = &self.title else {
            return true;
        };

        title_pat.0.is_match(title)
    }

    pub fn description_matches(&self, description: &str) -> bool {
        let Some(description_pat) = &self.description else {
            return true;
        };

        description_pat.0.is_match(description)
    }

    pub fn mod_loader_matches(&self, mod_loader: &ModLoader) -> bool {
        let Some(mod_loaders) = &self.mod_loaders else {
            return true;
        };

        mod_loaders.iter().any(|p| p == mod_loader)
    }

    pub fn game_version_matches(&self, version: &str) -> bool {
        let Some(versions) = &self.versions else {
            return true;
        };

        versions.iter().any(|p| p.matches(version))
    }

    pub fn is_empty(&self) -> bool {
        self.mod_loaders.is_none() && self.versions.is_none()
    }
}

macro_rules! impl_serde_for_parse {
    ($t:ty) => {
        impl Serialize for $t {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(&self.0.to_string())
            }
        }

        impl<'de> Deserialize<'de> for $t {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                deserializer.deserialize_str(ParseVisitor::<Self>(PhantomData))
            }
        }
    };
}

struct ParseVisitor<T>(PhantomData<T>);

impl<'de, T> Visitor<'de> for ParseVisitor<T>
where
    T: FromStr,
    <T as FromStr>::Err: std::fmt::Display,
{
    type Value = T;

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

#[derive(Clone)]
pub struct Regex(regex::Regex);

impl fmt::Debug for Regex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Regex").field(&self.0.to_string()).finish()
    }
}

impl fmt::Display for Regex {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Regex {
    type Err = regex::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(regex::Regex::new(s)?))
    }
}

impl_serde_for_parse!(Regex);

#[derive(Clone)]
pub struct Version(semver::VersionReq);

impl fmt::Debug for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("Version").field(&self.0.to_string()).finish()
    }
}

impl Version {
    pub fn matches(&self, s: &str) -> bool {
        let (rest, tag) = match s.find('-') {
            Some(index) => s.split_at(index),
            None => (s, ""),
        };

        let tag = if tag.is_empty() {
            None
        } else {
            Prerelease::new(&tag[1..])
                .inspect_err(|e| println!("WARN: semver tag parse error: {e}"))
                .ok()
        };

        fn find_split(s: &str) -> (&str, &str) {
            match s.find('.') {
                Some(index) => {
                    let (s, rest) = s.split_at(index);
                    (s, &rest[1..])
                }
                None => (s, ""),
            }
        }

        let (major, rest) = find_split(rest);
        let (minor, rest) = find_split(rest);
        let (patch, rest) = find_split(rest);

        if !rest.is_empty() {
            println!("WARN: semver parse error ({s:?}): unexpected eof, discarded data ({rest:?})");
        }

        fn parse_part(s: &str) -> u64 {
            if s.is_empty() {
                return 0;
            }
            s.parse().ok().unwrap_or(0)
        }

        let major = parse_part(major);
        let minor = parse_part(minor);
        let patch = parse_part(patch);

        let version = {
            let mut version = semver::Version::new(major, minor, patch);
            if let Some(tag) = tag {
                version.pre = tag;
            };
            version
        };

        self.0.matches(&version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for Version {
    type Err = semver::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(semver::VersionReq::parse(s)?))
    }
}

impl_serde_for_parse!(Version);

#[derive(
    Deserialize, Serialize, Debug, Display, Clone, Copy, PartialEq, Eq, clap::ValueEnum, Hash,
)]
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
